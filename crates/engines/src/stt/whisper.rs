use std::sync::Arc;

use async_trait::async_trait;
use kea_infer::{
    AudioPcm as InferAudioPcm, ModelRegistry, ModelStorage, WhisperInference, WhisperOpts,
};

use crate::stt::audio::{resample_to_rate, STT_SAMPLE_RATE_HZ};
use crate::traits::{AudioPcm, EngineCaps, EngineError, SttEngine, SttOpts, Transcript};

pub struct WhisperSttEngine {
    inference: Arc<dyn WhisperInference>,
    storage: Arc<ModelStorage>,
}

impl WhisperSttEngine {
    pub fn new(inference: Arc<dyn WhisperInference>, storage: Arc<ModelStorage>) -> Self {
        Self { inference, storage }
    }
}

#[async_trait]
impl SttEngine for WhisperSttEngine {
    fn id(&self) -> &str {
        "whisper"
    }

    /// What the picker may offer — retired entries excluded. They still
    /// *load*: `transcribe` below checks the file on disk, not this list, so
    /// a binding made before a model was retired keeps working.
    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: ModelRegistry::offered(kea_infer::ModelKind::Whisper)
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
        let samples = resample_to_rate(&audio.samples, audio.sample_rate_hz, STT_SAMPLE_RATE_HZ);
        let pcm = InferAudioPcm {
            samples,
            sample_rate_hz: STT_SAMPLE_RATE_HZ,
        };

        let whisper_opts = WhisperOpts {
            language: opts.language,
            vocabulary: opts.vocabulary,
        };

        let text = self
            .inference
            .transcribe(pcm, &model_path, whisper_opts)
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;

        Ok(Transcript { text })
    }
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

    /// A retired model is not offered, but a binding that already names one
    /// still has to transcribe — the check that matters is the file on disk.
    #[tokio::test]
    async fn a_retired_model_is_unlisted_but_still_loadable() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        std::fs::write(storage.path_for("ggml-medium.en"), b"x").unwrap();
        let engine = WhisperSttEngine::new(Arc::new(FakeWhisperInference), storage);

        assert!(!engine
            .capabilities()
            .models
            .contains(&"ggml-medium.en".to_string()));
        assert!(engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 16_000],
                    sample_rate_hz: 16_000,
                },
                SttOpts {
                    model: Some("ggml-medium.en".into()),
                    ..Default::default()
                },
            )
            .await
            .is_ok());
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
}
