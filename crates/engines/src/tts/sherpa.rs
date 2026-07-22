//! Local TTS engine backed by injectable [`SherpaTtsInference`] (sherpa-onnx VITS/Piper).

use std::sync::Arc;

use async_trait::async_trait;
use kea_infer::{ModelRegistry, ModelStorage, SherpaTtsInference};

use crate::traits::{AudioPcm, EngineCaps, EngineError, TtsEngine, TtsOpts};

pub struct LocalTtsEngine {
    inference: Arc<dyn SherpaTtsInference>,
    storage: Arc<ModelStorage>,
}

impl LocalTtsEngine {
    pub fn new(inference: Arc<dyn SherpaTtsInference>, storage: Arc<ModelStorage>) -> Self {
        Self {
            inference,
            storage,
        }
    }
}

#[async_trait]
impl TtsEngine for LocalTtsEngine {
    fn id(&self) -> &str {
        "sherpa-tts"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: ModelRegistry::tts_catalog()
                .into_iter()
                .map(|m| m.id)
                .collect(),
        }
    }

    async fn synthesize(&self, text: &str, opts: TtsOpts) -> Result<AudioPcm, EngineError> {
        if text.trim().is_empty() {
            return Err(EngineError::Config("empty text".into()));
        }

        let default_model = ModelRegistry::tts_catalog()
            .first()
            .map(|m| m.id.clone())
            .unwrap_or_else(|| "vits-piper-en-us-lessac-medium".into());
        let model_id = opts.model.as_deref().unwrap_or(&default_model);

        if !self.storage.is_onnx_installed(model_id) {
            return Err(EngineError::ModelNotInstalled(format!(
                "local TTS model not installed: {model_id}"
            )));
        }

        let model_dir = self.storage.onnx_dir_for(model_id);
        let pcm = self
            .inference
            .synthesize(text, &model_dir)
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;

        Ok(AudioPcm {
            samples: pcm.samples,
            sample_rate_hz: pcm.sample_rate_hz,
        })
    }
}

#[cfg(feature = "tts-local")]
pub fn register_sherpa_tts_engine(
    reg: &mut crate::registry::EngineRegistry,
    inference: Arc<dyn SherpaTtsInference>,
    storage: Arc<ModelStorage>,
) {
    reg.register_tts(Arc::new(LocalTtsEngine::new(inference, storage)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_infer::{AudioPcm as InferAudioPcm, SherpaTtsInference};
    use std::path::Path;

    struct FakeSherpaTtsInference;

    #[async_trait]
    impl SherpaTtsInference for FakeSherpaTtsInference {
        async fn synthesize(
            &self,
            text: &str,
            _model_dir: &Path,
        ) -> Result<InferAudioPcm, kea_infer::InferError> {
            Ok(InferAudioPcm {
                samples: vec![0.0; text.len() * 100],
                sample_rate_hz: 22_050,
            })
        }
    }

    #[tokio::test]
    async fn local_tts_engine_returns_pcm() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let model_dir = storage.onnx_dir_for("vits-piper-en-us-lessac-medium");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokens.txt"), b"tok").unwrap();

        let engine = LocalTtsEngine::new(Arc::new(FakeSherpaTtsInference), storage);
        let pcm = engine
            .synthesize("read aloud", TtsOpts::default())
            .await
            .unwrap();
        assert_eq!(pcm.sample_rate_hz, 22_050);
        assert_eq!(pcm.samples.len(), 1000);
    }

    #[tokio::test]
    async fn local_tts_errors_when_model_missing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let engine = LocalTtsEngine::new(Arc::new(FakeSherpaTtsInference), storage);
        let err = engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not installed"));
    }
}
