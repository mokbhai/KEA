//! Local TTS engine backed by injectable [`SherpaTtsInference`].
//!
//! One engine for four bundle shapes (Piper/VITS, Kokoro, Kitten, Matcha):
//! they share a storage root, a download path and a picker section, and differ
//! only in which sherpa model config loads them — which the catalog already
//! records, so the engine dispatches on the entry rather than sniffing the
//! directory.
//!
//! Matcha is the one voice whose files are two downloads: an acoustic model
//! and the shared HiFiGAN vocoder, which is its own catalog row. Pairing them
//! is this engine's job, because it is the layer that has the storage root —
//! the same split as `DiarizationModels::locate`.

use std::sync::Arc;

use async_trait::async_trait;
use kea_infer::{
    clamp_tts_speed, ModelRegistry, ModelStorage, OnnxModelKind, SherpaTtsInference, TtsModelPaths,
    TtsSynthOpts, MATCHA_VOCODER_FILE, MATCHA_VOCODER_ID,
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
        // Voices, not the whole TTS catalog: the Matcha vocoder installs into
        // the same root and would otherwise be picked as "the first installed
        // entry" and then refuse to speak.
        let catalog = ModelRegistry::tts_voices();
        catalog
            .iter()
            .find(|entry| self.storage.is_onnx_installed(&entry.id))
            .or_else(|| catalog.first())
            .map(|entry| entry.id.clone())
            .unwrap_or_else(|| "vits-piper-en-us-lessac-medium".into())
    }

    /// Where the files for this voice live, pairing in the vocoder when the
    /// voice needs one.
    ///
    /// A Matcha voice with no vocoder on disk is refused here rather than
    /// allowed to reach sherpa: `ModelNotInstalled` is the variant whose
    /// message tells the user to download something, and the something is
    /// named.
    fn paths_for(&self, model_id: &str, kind: OnnxModelKind) -> Result<TtsModelPaths, EngineError> {
        let paths = TtsModelPaths::new(self.storage.onnx_dir_for(model_id));
        if kind != OnnxModelKind::TtsMatcha {
            return Ok(paths);
        }
        let vocoder_dir = self.storage.onnx_dir_for(MATCHA_VOCODER_ID);
        if !vocoder_dir.join(MATCHA_VOCODER_FILE).is_file() {
            return Err(EngineError::ModelNotInstalled(format!(
                "the voice '{model_id}' also needs the vocoder '{MATCHA_VOCODER_ID}'"
            )));
        }
        Ok(paths.with_vocoder(vocoder_dir))
    }
}

#[async_trait]
impl TtsEngine for LocalTtsEngine {
    fn id(&self) -> &str {
        "sherpa-tts"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            // What this engine can be asked to *speak with*. The vocoder is
            // offered for download elsewhere; binding to it could only fail.
            models: ModelRegistry::tts_voices()
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

        let paths = self.paths_for(model_id, kind)?;
        let pcm = self
            .inference
            .synthesize(text, &paths, kind, synth_opts)
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
    use std::sync::Mutex;

    /// Records what the engine resolved, which is the whole contract here —
    /// the fake produces no real audio, but it is the only place the kind,
    /// the speaker id and the rate can be observed.
    #[derive(Default)]
    struct FakeSherpaTtsInference {
        seen: Mutex<Vec<(TtsModelPaths, OnnxModelKind, TtsSynthOpts)>>,
    }

    #[async_trait]
    impl SherpaTtsInference for FakeSherpaTtsInference {
        async fn synthesize(
            &self,
            text: &str,
            paths: &TtsModelPaths,
            kind: OnnxModelKind,
            opts: TtsSynthOpts,
        ) -> Result<InferAudioPcm, kea_infer::InferError> {
            self.seen.lock().unwrap().push((paths.clone(), kind, opts));
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

    /// The vocoder is a bare `.onnx`, so its marker is the file itself.
    fn install_vocoder(storage: &ModelStorage) {
        let dir = storage.onnx_dir_for(MATCHA_VOCODER_ID);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MATCHA_VOCODER_FILE), b"w").unwrap();
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
            let (paths, kind, _) = inference.seen.lock().unwrap().last().unwrap().clone();
            assert_eq!(kind, expected, "wrong bundle shape for {model_id}");
            assert_eq!(paths.model_dir, storage.onnx_dir_for(model_id));
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
            inference.seen.lock().unwrap()[0].0.model_dir,
            storage.onnx_dir_for("vits-piper-en-us-lessac-medium")
        );

        // Once the recommended voice is there, it is the one that is used.
        install(&storage, "kitten-nano-en-v0.2");
        engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap();
        assert_eq!(
            inference.seen.lock().unwrap().last().unwrap().0.model_dir,
            storage.onnx_dir_for("kitten-nano-en-v0.2")
        );
    }

    /// Matcha is two downloads. The engine is the layer that owns the storage
    /// root, so pairing them is its job — and the acoustic model's directory
    /// must not be mistaken for the vocoder's.
    #[tokio::test]
    async fn a_matcha_voice_is_handed_both_of_its_directories() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        install(&storage, "matcha-icefall-en-us-ljspeech");
        install_vocoder(&storage);
        let inference = Arc::new(FakeSherpaTtsInference::default());
        let engine = LocalTtsEngine::new(inference.clone(), storage.clone());

        engine
            .synthesize(
                "hello",
                TtsOpts {
                    model: Some("matcha-icefall-en-us-ljspeech".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let (paths, kind, _) = inference.seen.lock().unwrap().last().unwrap().clone();
        assert_eq!(kind, OnnxModelKind::TtsMatcha);
        assert_eq!(
            paths.model_dir,
            storage.onnx_dir_for("matcha-icefall-en-us-ljspeech")
        );
        assert_eq!(
            paths.vocoder_dir,
            Some(storage.onnx_dir_for(MATCHA_VOCODER_ID))
        );
    }

    /// Half an install is the interesting case: the voice is on disk, so the
    /// ordinary "not installed" check passes, and the missing piece has to be
    /// named or the user is told a voice they just downloaded does not work.
    #[tokio::test]
    async fn matcha_without_its_vocoder_names_the_missing_download() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        install(&storage, "matcha-icefall-en-us-ljspeech");
        let inference = Arc::new(FakeSherpaTtsInference::default());
        let engine = LocalTtsEngine::new(inference.clone(), storage);

        let err = engine
            .synthesize(
                "hello",
                TtsOpts {
                    model: Some("matcha-icefall-en-us-ljspeech".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::ModelNotInstalled(_)), "{err}");
        assert!(err.to_string().contains(MATCHA_VOCODER_ID), "{err}");
        // And nothing was synthesized in another voice instead.
        assert!(inference.seen.lock().unwrap().is_empty());
    }

    /// The vocoder lives in the TTS catalog so it can be downloaded, but it
    /// is not something to speak with: it must never be offered as a model,
    /// and never chosen as the fallback voice just because it is on disk.
    #[tokio::test]
    async fn the_vocoder_is_never_treated_as_a_voice() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        install_vocoder(&storage);
        let inference = Arc::new(FakeSherpaTtsInference::default());
        let engine = LocalTtsEngine::new(inference.clone(), storage.clone());

        assert!(!engine
            .capabilities()
            .models
            .contains(&MATCHA_VOCODER_ID.to_string()));

        // With only the vocoder on disk there is no installed voice, so the
        // fallback is the recommended download — and it is reported missing,
        // not silently loaded as a voice.
        let err = engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("kitten-nano-en-v0.2"), "{err}");

        install(&storage, "kitten-nano-en-v0.2");
        engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap();
        assert_eq!(
            inference.seen.lock().unwrap().last().unwrap().0.model_dir,
            storage.onnx_dir_for("kitten-nano-en-v0.2")
        );
    }
}
