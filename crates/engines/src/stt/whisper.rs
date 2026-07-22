use std::sync::Arc;

use async_trait::async_trait;
use kea_infer::{AudioPcm as InferAudioPcm, ModelRegistry, ModelStorage, WhisperInference, WhisperOpts};

use crate::traits::{AudioPcm, EngineCaps, EngineError, SttEngine, SttOpts, Transcript};

const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;

pub struct WhisperSttEngine {
    inference: Arc<dyn WhisperInference>,
    storage: Arc<ModelStorage>,
}

impl WhisperSttEngine {
    pub fn new(inference: Arc<dyn WhisperInference>, storage: Arc<ModelStorage>) -> Self {
        Self {
            inference,
            storage,
        }
    }
}

#[async_trait]
impl SttEngine for WhisperSttEngine {
    fn id(&self) -> &str {
        "whisper"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: ModelRegistry::whisper_catalog()
                .into_iter()
                .map(|m| m.id)
                .collect(),
        }
    }

    async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError> {
        let model_id = opts
            .model
            .as_deref()
            .ok_or_else(|| EngineError::Config("whisper requires a model id".into()))?;

        if !self.storage.is_installed(model_id) {
            return Err(EngineError::ModelNotInstalled(format!(
                "whisper model not installed: {model_id}"
            )));
        }

        let model_path = self.storage.path_for(model_id);
        let samples = resample_to_rate(&audio.samples, audio.sample_rate_hz, WHISPER_SAMPLE_RATE_HZ);
        let pcm = InferAudioPcm {
            samples,
            sample_rate_hz: WHISPER_SAMPLE_RATE_HZ,
        };

        let whisper_opts = WhisperOpts {
            language: opts.language,
        };

        let text = self
            .inference
            .transcribe(pcm, &model_path, whisper_opts)
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

#[cfg(feature = "whisper")]
pub fn register_whisper_stt_engine(
    reg: &mut crate::registry::EngineRegistry,
    inference: Arc<dyn WhisperInference>,
    storage: Arc<ModelStorage>,
) {
    reg.register_stt(Arc::new(WhisperSttEngine::new(inference, storage)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_infer::WhisperInference;
    use std::path::Path;

    struct FakeWhisperInference;

    #[async_trait]
    impl WhisperInference for FakeWhisperInference {
        async fn transcribe(
            &self,
            pcm: InferAudioPcm,
            _model_path: &Path,
            _opts: WhisperOpts,
        ) -> Result<String, kea_infer::InferError> {
            Ok(format!("whisper heard {} samples", pcm.samples.len()))
        }
    }

    #[tokio::test]
    async fn whisper_engine_uses_inference_trait() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let model_path = storage.path_for("ggml-base.en");
        std::fs::write(&model_path, b"x").unwrap();
        let engine = WhisperSttEngine::new(Arc::new(FakeWhisperInference), storage.clone());
        let out = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 16_000],
                    sample_rate_hz: 16_000,
                },
                SttOpts {
                    model: Some("ggml-base.en".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(out.text.contains("16000"));
    }

    #[tokio::test]
    async fn whisper_engine_errors_when_model_missing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let engine = WhisperSttEngine::new(Arc::new(FakeWhisperInference), storage);
        let err = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 100],
                    sample_rate_hz: 16_000,
                },
                SttOpts {
                    model: Some("ggml-base.en".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not installed"));
    }

    #[test]
    fn resample_halves_sample_count_when_halving_rate() {
        let samples: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        let out = resample_to_rate(&samples, 48_000, 24_000);
        assert_eq!(out.len(), 50);
    }
}
