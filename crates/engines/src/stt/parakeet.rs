//! Parakeet STT engine backed by injectable [`SherpaSttInference`].
//!
//! Real ONNX inference is provided by [`SherpaOnnxSttInference`] when the `sherpa`
//! feature is enabled on `kea-infer`. Engine logic is tested with fakes under default
//! features.
//!
//! ## D6 `ort` fallback
//!
//! If sherpa-onnx Parakeet bindings block release (native build failures, API drift,
//! or packaging constraints), switch the composition root to a raw [`ort`] session
//! implementation of [`SherpaSttInference`] that loads the same NeMo-exported ONNX
//! encoder/decoder/joiner + `tokens.txt` bundle. Export steps and trait seam are
//! documented in `docs/cross-platform/plans/CONTRACTS.md` (Parakeet ort fallback).
//! The engine layer above this trait does not change.

use std::sync::Arc;

use async_trait::async_trait;
use kea_infer::{ModelRegistry, ModelStorage, SherpaSttInference, WhisperOpts};

use crate::traits::{AudioPcm, EngineCaps, EngineError, SttEngine, SttOpts, Transcript};

const PARAKEET_SAMPLE_RATE_HZ: u32 = 16_000;

pub struct ParakeetSttEngine {
    inference: Arc<dyn SherpaSttInference>,
    storage: Arc<ModelStorage>,
}

impl ParakeetSttEngine {
    pub fn new(inference: Arc<dyn SherpaSttInference>, storage: Arc<ModelStorage>) -> Self {
        Self {
            inference,
            storage,
        }
    }
}

#[async_trait]
impl SttEngine for ParakeetSttEngine {
    fn id(&self) -> &str {
        "parakeet"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: ModelRegistry::parakeet_catalog()
                .into_iter()
                .map(|m| m.id)
                .collect(),
        }
    }

    async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError> {
        let model_id = opts
            .model
            .as_deref()
            .ok_or_else(|| EngineError::Config("parakeet requires a model id".into()))?;

        if !self.storage.is_onnx_installed(model_id) {
            return Err(EngineError::ModelNotInstalled(format!(
                "parakeet model not installed: {model_id}"
            )));
        }

        let model_dir = self.storage.onnx_dir_for(model_id);
        let samples = resample_to_rate(&audio.samples, audio.sample_rate_hz, PARAKEET_SAMPLE_RATE_HZ);
        let pcm = kea_infer::AudioPcm {
            samples,
            sample_rate_hz: PARAKEET_SAMPLE_RATE_HZ,
        };

        let whisper_opts = WhisperOpts {
            language: opts.language,
        };

        let text = self
            .inference
            .transcribe_parakeet(pcm, &model_dir, whisper_opts)
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;

        Ok(Transcript { text })
    }
}

fn resample_to_rate(samples: &[f32], src_rate_hz: u32, dst_rate_hz: u32) -> Vec<f32> {
    if src_rate_hz == dst_rate_hz || samples.is_empty() {
        return samples.to_vec();
    }

    let ratio = src_rate_hz as f64 / dst_rate_hz as f64;
    let out_len = ((samples.len() as f64) / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;
        let s0 = samples.get(idx).copied().unwrap_or(0.0);
        let s1 = samples.get(idx + 1).copied().unwrap_or(s0);
        out.push(s0 + (s1 - s0) * frac);
    }

    out
}

#[cfg(feature = "parakeet")]
pub fn register_parakeet_stt_engine(
    reg: &mut crate::registry::EngineRegistry,
    inference: Arc<dyn SherpaSttInference>,
    storage: Arc<ModelStorage>,
) {
    reg.register_stt(Arc::new(ParakeetSttEngine::new(inference, storage)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_infer::{AudioPcm as InferAudioPcm, SherpaSttInference, WhisperOpts};
    use std::path::Path;

    struct FakeSherpaSttInference;

    #[async_trait]
    impl SherpaSttInference for FakeSherpaSttInference {
        async fn transcribe_parakeet(
            &self,
            pcm: InferAudioPcm,
            _model_dir: &Path,
            _opts: WhisperOpts,
        ) -> Result<String, kea_infer::InferError> {
            Ok(format!("parakeet: {} samples", pcm.samples.len()))
        }
    }

    #[tokio::test]
    async fn parakeet_stt_uses_injected_inference() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let model_dir = storage.onnx_dir_for("parakeet-tdt-0.6b-v2");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokens.txt"), b"tok").unwrap();

        let engine = ParakeetSttEngine::new(Arc::new(FakeSherpaSttInference), storage);
        let out = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 1600],
                    sample_rate_hz: 16_000,
                },
                SttOpts {
                    model: Some("parakeet-tdt-0.6b-v2".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(out.text.contains("parakeet"));
        assert!(out.text.contains("1600"));
    }

    #[tokio::test]
    async fn parakeet_engine_errors_when_model_missing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let engine = ParakeetSttEngine::new(Arc::new(FakeSherpaSttInference), storage);
        let err = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 100],
                    sample_rate_hz: 16_000,
                },
                SttOpts {
                    model: Some("parakeet-tdt-0.6b-v2".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not installed"));
    }
}
