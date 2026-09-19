//! The offline local STT engine, backed by injectable [`SherpaSttInference`].
//!
//! Real ONNX inference is provided by [`SherpaOnnxSttInference`] when the `sherpa`
//! feature is enabled on `kea-infer`. Engine logic is tested with fakes under default
//! features.
//!
//! ## Why it is still called `parakeet`
//!
//! It serves every bundle in `ModelKind::Parakeet` — the NeMo transducers and
//! the Moonshine recognizers — because they install the same way, bind to the
//! same `stt` slot and differ only in which sherpa model config loads them.
//! The id stays `"parakeet"` regardless: it is persisted in every user's
//! capability binding, and renaming it would silently unbind whoever had
//! chosen a local recognizer. The catalog entry's [`OnnxModelKind`] is what
//! actually decides how the bundle is loaded, and it travels down to the
//! inference layer rather than being guessed from the directory.
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
use kea_infer::{ModelRegistry, ModelStorage, SherpaSttInference, SttHotwords};

use crate::stt::audio::{resample_to_rate, STT_SAMPLE_RATE_HZ};
use crate::stt::segments::from_infer;
use crate::traits::{AudioPcm, EngineCaps, EngineError, SttEngine, SttOpts, Transcript};

pub struct ParakeetSttEngine {
    inference: Arc<dyn SherpaSttInference>,
    storage: Arc<ModelStorage>,
}

impl ParakeetSttEngine {
    pub fn new(inference: Arc<dyn SherpaSttInference>, storage: Arc<ModelStorage>) -> Self {
        Self { inference, storage }
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

        // The catalog, not the directory listing, says which sherpa config
        // this bundle loads through. An id that is not in the catalog cannot
        // be loaded at all, so it is refused here rather than at the point
        // where four unknown `.onnx` files fail to become a recognizer.
        let entry = ModelRegistry::find_parakeet(model_id)
            .ok_or_else(|| EngineError::Config(format!("unknown local stt model: {model_id}")))?;

        if !self.storage.is_onnx_entry_installed(&entry) {
            return Err(EngineError::ModelNotInstalled(format!(
                "parakeet model not installed: {model_id}"
            )));
        }

        let model_dir = self.storage.onnx_dir_for(model_id);
        let samples = resample_to_rate(&audio.samples, audio.sample_rate_hz, STT_SAMPLE_RATE_HZ);
        let pcm = kea_infer::AudioPcm {
            samples,
            sample_rate_hz: STT_SAMPLE_RATE_HZ,
        };

        // No language is passed on: the NeMo transducer behind this trait has
        // no language setting, so `opts.language` could only be dropped.
        //
        // The vocabulary is a different case and does go down: it reaches
        // sherpa's `hotwords_file` when the bundle can encode it, and the
        // inference layer logs why when it cannot (see `plan_hotwords`).
        // Normalized here, once, so an empty or whitespace-only list arrives
        // as "no hotwords" rather than as a reason to switch decoders.
        let hotwords = SttHotwords::new(opts.vocabulary);
        let result = self
            .inference
            .transcribe(pcm, &model_dir, entry.kind, &hotwords)
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;

        Ok(from_infer(result))
    }
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
    use kea_infer::{AudioPcm as InferAudioPcm, OnnxModelKind, SherpaSttInference};
    use std::path::Path;
    use std::sync::Mutex;

    /// Records the hotwords it was handed: the only place the vocabulary can
    /// be observed reaching the inference layer rather than stopping at the
    /// engine boundary.
    #[derive(Default)]
    struct FakeSherpaSttInference {
        seen: Mutex<Vec<SttHotwords>>,
        kinds: Mutex<Vec<OnnxModelKind>>,
    }

    #[async_trait]
    impl SherpaSttInference for FakeSherpaSttInference {
        async fn transcribe(
            &self,
            pcm: InferAudioPcm,
            _model_dir: &Path,
            kind: OnnxModelKind,
            hotwords: &SttHotwords,
        ) -> Result<kea_infer::SttResult, kea_infer::InferError> {
            self.seen.lock().unwrap().push(hotwords.clone());
            self.kinds.lock().unwrap().push(kind);
            Ok(kea_infer::SttResult::text_only(format!(
                "parakeet: {} samples",
                pcm.samples.len()
            )))
        }
    }

    #[tokio::test]
    async fn parakeet_stt_uses_injected_inference() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let model_dir = storage.onnx_dir_for("parakeet-tdt-0.6b-v2");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokens.txt"), b"tok").unwrap();

        let engine = ParakeetSttEngine::new(Arc::new(FakeSherpaSttInference::default()), storage);
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

    /// `SttOpts::vocabulary` is filled from the user's enabled terms and used
    /// to be dropped at this boundary. It has to arrive normalized at the
    /// inference layer, which is where it becomes sherpa's `hotwords_file`.
    #[tokio::test]
    async fn the_vocabulary_reaches_the_inference_layer() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let model_dir = storage.onnx_dir_for("parakeet-tdt-0.6b-v2");
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokens.txt"), b"tok").unwrap();

        let inference = Arc::new(FakeSherpaSttInference::default());
        let engine = ParakeetSttEngine::new(inference.clone(), storage);

        for (vocabulary, expected) in [
            (
                vec!["KittyClaw".to_string(), "KEA".to_string()],
                vec!["KittyClaw", "KEA"],
            ),
            // Trimmed, de-duplicated and emptied out before it travels.
            (
                vec![" KEA ".to_string(), "KEA".to_string(), "  ".to_string()],
                vec!["KEA"],
            ),
            (Vec::new(), Vec::new()),
        ] {
            engine
                .transcribe(
                    AudioPcm {
                        samples: vec![0.0; 160],
                        sample_rate_hz: 16_000,
                    },
                    SttOpts {
                        model: Some("parakeet-tdt-0.6b-v2".into()),
                        vocabulary,
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            let seen = inference.seen.lock().unwrap().last().unwrap().clone();
            assert_eq!(seen.terms(), expected.as_slice());
        }
    }

    /// The Moonshine rows share this engine, this storage root and this
    /// binding — what has to differ is the sherpa config, and the only way
    /// that reaches the loader is the catalog's `OnnxModelKind` travelling
    /// down. If it stopped here, a Moonshine bundle would be loaded as a
    /// transducer and fail with four files it cannot name.
    #[tokio::test]
    async fn the_catalogs_model_shape_reaches_the_inference_layer() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let inference = Arc::new(FakeSherpaSttInference::default());
        let engine = ParakeetSttEngine::new(inference.clone(), storage.clone());

        for (model_id, expected) in [
            ("moonshine-tiny-en", OnnxModelKind::Moonshine),
            ("parakeet-tdt-0.6b-v2", OnnxModelKind::Parakeet),
        ] {
            let model_dir = storage.onnx_dir_for(model_id);
            std::fs::create_dir_all(&model_dir).unwrap();
            std::fs::write(model_dir.join("tokens.txt"), b"tok").unwrap();

            engine
                .transcribe(
                    AudioPcm {
                        samples: vec![0.0; 160],
                        sample_rate_hz: 16_000,
                    },
                    SttOpts {
                        model: Some(model_id.into()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            assert_eq!(*inference.kinds.lock().unwrap().last().unwrap(), expected);
        }
    }

    /// Both Moonshine sizes are offered, and the engine advertises them.
    #[test]
    fn the_engine_advertises_every_local_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let engine = ParakeetSttEngine::new(
            Arc::new(FakeSherpaSttInference::default()),
            Arc::new(ModelStorage::new(dir.path().to_path_buf())),
        );
        let models = engine.capabilities().models;
        for id in [
            "moonshine-tiny-en",
            "moonshine-base-en",
            "parakeet-tdt-0.6b-v2",
        ] {
            assert!(
                models.iter().any(|m| m == id),
                "{id} missing from {models:?}"
            );
        }
    }

    /// A model id that is not in the catalog cannot be loaded, so it is
    /// refused by name rather than reaching the loader as an empty directory.
    #[tokio::test]
    async fn an_unknown_model_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let engine = ParakeetSttEngine::new(
            Arc::new(FakeSherpaSttInference::default()),
            Arc::new(ModelStorage::new(dir.path().to_path_buf())),
        );
        let err = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 100],
                    sample_rate_hz: 16_000,
                },
                SttOpts {
                    model: Some("not-a-model".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown local stt model"), "{err}");
    }

    #[tokio::test]
    async fn parakeet_engine_errors_when_model_missing() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let engine = ParakeetSttEngine::new(Arc::new(FakeSherpaSttInference::default()), storage);
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
