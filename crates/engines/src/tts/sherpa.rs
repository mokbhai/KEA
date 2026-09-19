//! Local TTS engine backed by injectable [`SherpaTtsInference`].
//!
//! One engine for three bundle shapes (Piper/VITS, Kokoro, Kitten): they share
//! a storage root, a download path and a picker section, and differ only in
//! which sherpa model config loads them — which the catalog already records,
//! so the engine dispatches on the entry rather than sniffing the directory.

use std::sync::Arc;

use async_trait::async_trait;
use kea_infer::{
    clamp_tts_speed, ModelKind, ModelRegistry, ModelStorage, OnnxModelKind, SherpaTtsInference,
    TtsSynthOpts,
};

use crate::traits::{AudioPcm, EngineCaps, EngineError, TtsEngine, TtsOpts};

pub struct LocalTtsEngine {
    inference: Arc<dyn SherpaTtsInference>,
    storage: Arc<ModelStorage>,
}

impl LocalTtsEngine {
    pub fn new(inference: Arc<dyn SherpaTtsInference>, storage: Arc<ModelStorage>) -> Self {
        Self { inference, storage }
    }

    /// The voice to use when neither the binding nor the settings name one.
    ///
    /// The first *installed* catalog entry, not simply the first entry: the
    /// head of the catalog is the recommended download, and an install that
    /// predates it would otherwise fall back to a model that is not on disk
    /// and fail with "not installed" for a voice the user never chose.
    fn default_model(&self) -> String {
        let catalog = ModelRegistry::offered(ModelKind::Tts);
        catalog
            .iter()
            .find(|entry| self.storage.is_onnx_installed(&entry.id))
            .or_else(|| catalog.first())
            .map(|entry| entry.id.clone())
            .unwrap_or_else(|| "vits-piper-en-us-lessac-medium".into())
    }
}

#[async_trait]
impl TtsEngine for LocalTtsEngine {
    fn id(&self) -> &str {
        "sherpa-tts"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: ModelRegistry::offered(ModelKind::Tts)
                .into_iter()
                .map(|m| m.id)
                .collect(),
        }
    }

    async fn synthesize(&self, text: &str, opts: TtsOpts) -> Result<AudioPcm, EngineError> {
        if text.trim().is_empty() {
            return Err(EngineError::Config("empty text".into()));
        }

        let default_model = self.default_model();
        let model_id = opts.model.as_deref().unwrap_or(&default_model);

        if !self.storage.is_onnx_installed(model_id) {
            return Err(EngineError::ModelNotInstalled(format!(
                "local TTS model not installed: {model_id}"
            )));
        }

        // An id that is on disk but not in the catalog is a bundle someone
        // placed there by hand. VITS is the only shape that was ever
        // installable that way, and refusing outright would break it.
        let kind = ModelRegistry::find_tts(model_id)
            .map(|entry| entry.kind)
            .unwrap_or_else(|| {
                tracing::warn!(
                    model = %model_id,
                    "local voice is not in the catalog; assuming a VITS bundle"
                );
                OnnxModelKind::TtsVits
            });

        let synth_opts = TtsSynthOpts {
            // The setting holds a name; the bundle wants an index.
            sid: ModelRegistry::voice_sid(model_id, opts.voice.as_deref()),
            speed: clamp_tts_speed(opts.speed.unwrap_or(1.0)),
        };

        let model_dir = self.storage.onnx_dir_for(model_id);
        let pcm = self
            .inference
            .synthesize(text, &model_dir, kind, synth_opts)
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
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Records what the engine resolved, which is the whole contract here —
    /// the fake produces no real audio, but it is the only place the kind,
    /// the speaker id and the rate can be observed.
    #[derive(Default)]
    struct FakeSherpaTtsInference {
        seen: Mutex<Vec<(PathBuf, OnnxModelKind, TtsSynthOpts)>>,
    }

    #[async_trait]
    impl SherpaTtsInference for FakeSherpaTtsInference {
        async fn synthesize(
            &self,
            text: &str,
            model_dir: &Path,
            kind: OnnxModelKind,
            opts: TtsSynthOpts,
        ) -> Result<InferAudioPcm, kea_infer::InferError> {
            self.seen
                .lock()
                .unwrap()
                .push((model_dir.to_path_buf(), kind, opts));
            Ok(InferAudioPcm {
                samples: vec![0.0; text.len() * 100],
                sample_rate_hz: 22_050,
            })
        }
    }

    /// Lays out an installed bundle (the installer's marker is tokens.txt).
    fn install(storage: &ModelStorage, model_id: &str) {
        let dir = storage.onnx_dir_for(model_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tokens.txt"), b"tok").unwrap();
    }

    #[tokio::test]
    async fn local_tts_engine_returns_pcm() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        install(&storage, "vits-piper-en-us-lessac-medium");

        let engine = LocalTtsEngine::new(Arc::new(FakeSherpaTtsInference::default()), storage);
        let pcm = engine
            .synthesize(
                "read aloud",
                TtsOpts {
                    model: Some("vits-piper-en-us-lessac-medium".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(pcm.sample_rate_hz, 22_050);
        assert_eq!(pcm.samples.len(), 1000);
    }

    #[tokio::test]
    async fn local_tts_errors_when_model_missing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let engine = LocalTtsEngine::new(Arc::new(FakeSherpaTtsInference::default()), storage);
        let err = engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not installed"));
    }

    /// The bundle shape comes from the catalog entry, not from the files on
    /// disk: loading a Kokoro bundle through the VITS config is the failure
    /// this dispatch exists to prevent.
    #[tokio::test]
    async fn each_model_is_loaded_through_its_own_bundle_shape() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let inference = Arc::new(FakeSherpaTtsInference::default());
        let engine = LocalTtsEngine::new(inference.clone(), storage.clone());

        for (model_id, expected) in [
            ("vits-piper-en-us-lessac-medium", OnnxModelKind::TtsVits),
            ("kokoro-en-v0.19", OnnxModelKind::TtsKokoro),
            ("kitten-nano-en-v0.2", OnnxModelKind::TtsKitten),
        ] {
            install(&storage, model_id);
            engine
                .synthesize(
                    "hello",
                    TtsOpts {
                        model: Some(model_id.into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let (dir, kind, _) = inference.seen.lock().unwrap().last().unwrap().clone();
            assert_eq!(kind, expected, "wrong bundle shape for {model_id}");
            assert_eq!(dir, storage.onnx_dir_for(model_id));
        }
    }

    /// The stored setting is a name; the bundle addresses speakers by index.
    /// Storing the index instead is what would make the picker read "10".
    #[tokio::test]
    async fn the_voice_name_is_resolved_to_a_speaker_id() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        install(&storage, "kokoro-en-v0.19");
        let inference = Arc::new(FakeSherpaTtsInference::default());
        let engine = LocalTtsEngine::new(inference.clone(), storage);

        for (voice, expected_sid) in [
            (Some("bm_lewis"), 10),
            (Some("af_bella"), 1),
            // A name from a model the user has since switched away from.
            (Some("alloy"), 0),
            (None, 0),
        ] {
            engine
                .synthesize(
                    "hello",
                    TtsOpts {
                        model: Some("kokoro-en-v0.19".into()),
                        voice: voice.map(str::to_string),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let opts = inference.seen.lock().unwrap().last().unwrap().2;
            assert_eq!(opts.sid, expected_sid, "wrong sid for {voice:?}");
        }
    }

    #[tokio::test]
    async fn the_rate_is_clamped_before_it_reaches_the_backend() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        install(&storage, "kitten-nano-en-v0.2");
        let inference = Arc::new(FakeSherpaTtsInference::default());
        let engine = LocalTtsEngine::new(inference.clone(), storage);

        for (asked, expected) in [
            (None, 1.0),
            (Some(1.25), 1.25),
            (Some(0.0), kea_infer::MIN_TTS_SPEED),
            (Some(50.0), kea_infer::MAX_TTS_SPEED),
        ] {
            engine
                .synthesize(
                    "hello",
                    TtsOpts {
                        model: Some("kitten-nano-en-v0.2".into()),
                        speed: asked,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let opts = inference.seen.lock().unwrap().last().unwrap().2;
            assert_eq!(opts.speed, expected, "wrong speed for {asked:?}");
        }
    }

    /// The catalog head is the recommended download, but an existing install
    /// predates it. Falling back to the first *installed* voice is what keeps
    /// "read this aloud" working for someone who upgraded rather than
    /// failing with "not installed" for a model they never chose.
    #[tokio::test]
    async fn the_fallback_voice_is_one_that_is_actually_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        install(&storage, "vits-piper-en-us-lessac-medium");
        let inference = Arc::new(FakeSherpaTtsInference::default());
        let engine = LocalTtsEngine::new(inference.clone(), storage.clone());

        engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap();
        assert_eq!(
            inference.seen.lock().unwrap()[0].0,
            storage.onnx_dir_for("vits-piper-en-us-lessac-medium")
        );

        // Once the recommended voice is there, it is the one that is used.
        install(&storage, "kitten-nano-en-v0.2");
        engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap();
        assert_eq!(
            inference.seen.lock().unwrap().last().unwrap().0,
            storage.onnx_dir_for("kitten-nano-en-v0.2")
        );
    }
}
