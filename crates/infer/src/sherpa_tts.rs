//! Local text-to-speech through sherpa-onnx.
//!
//! Three bundle shapes share one storage root, one download path and one
//! picker section but not one sherpa model config, so what this module really
//! owns is the mapping from [`OnnxModelKind`] to "which files, into which
//! config". The file-locating half is deliberately outside the `sherpa`
//! feature gate: it is path logic with no native dependency, and it is the
//! half that can be tested without a 100 MB bundle on disk.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::error::InferError;
use crate::registry::OnnxModelKind;
use crate::types::TtsSynthOpts;
use crate::whisper::AudioPcm;

#[async_trait]
pub trait SherpaTtsInference: Send + Sync {
    /// `kind` says which sherpa model config the bundle in `model_dir` has to
    /// be loaded through. It is a parameter rather than something sniffed from
    /// the directory because the catalog already knows it — and guessing it
    /// from the files present is exactly the heuristic this module replaced.
    async fn synthesize(
        &self,
        text: &str,
        model_dir: &Path,
        kind: OnnxModelKind,
        opts: TtsSynthOpts,
    ) -> Result<AudioPcm, InferError>;
}

/// The files one unpacked voice bundle contributes to a sherpa model config.
///
/// Every field that is `Option` is genuinely optional in at least one shipped
/// bundle: Piper has no `voices.bin`, Kitten no `dict/`, the English Kokoro no
/// lexicon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TtsBundle {
    pub model: PathBuf,
    pub tokens: PathBuf,
    /// espeak-ng phoneme data.
    pub data_dir: Option<PathBuf>,
    /// The packed speaker embedding table of a multi-speaker bundle.
    pub voices: Option<PathBuf>,
    /// jieba dictionary, only in the multilingual Kokoro bundle.
    pub dict_dir: Option<PathBuf>,
    /// Comma-joined lexicon paths, which is the form sherpa's `lexicon` field
    /// takes when a bundle ships more than one (the multilingual Kokoro ships
    /// one per language).
    pub lexicon: Option<String>,
}

/// The model filenames each bundle shape is allowed to use, best first.
///
/// An explicit list rather than "the first `.onnx` that is not an encoder":
/// that heuristic reads the directory in whatever order the filesystem hands
/// back and silently picks a *different* model the moment a bundle holds two,
/// which is a wrong voice with no error anywhere. The alternatives here are
/// the same model at different quantizations, so any of them is the right
/// answer and the order only expresses a preference.
fn model_filenames(kind: OnnxModelKind) -> &'static [&'static str] {
    match kind {
        OnnxModelKind::TtsKokoro => &["model.int8.onnx", "model.onnx", "model.fp16.onnx"],
        OnnxModelKind::TtsKitten => &["model.fp16.onnx", "model.int8.onnx", "model.onnx"],
        // Piper names the file after the voice ("en_US-lessac-medium.onnx"),
        // so there is no list to match against — see `find_sole_onnx`. The
        // recognizer bundles have no single model file at all: they are
        // encoder/decoder/joiner triples, found by their own finders.
        OnnxModelKind::TtsVits
        | OnnxModelKind::Parakeet
        | OnnxModelKind::StreamingZipformer
        | OnnxModelKind::SpeakerSegmentation
        | OnnxModelKind::SpeakerEmbedding => &[],
    }
}

fn require_file(dir: &Path, name: &str) -> Result<PathBuf, InferError> {
    let path = dir.join(name);
    if path.is_file() {
        Ok(path)
    } else {
        Err(InferError::Other(format!(
            "missing {name} in {}",
            dir.display()
        )))
    }
}

fn optional_dir(dir: &Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    path.is_dir().then_some(path)
}

fn named_model(dir: &Path, kind: OnnxModelKind) -> Result<PathBuf, InferError> {
    let names = model_filenames(kind);
    names
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            InferError::Other(format!(
                "no {kind:?} model in {} (expected one of: {})",
                dir.display(),
                names.join(", ")
            ))
        })
}

/// The one `.onnx` in a Piper bundle — and a refusal if there is more than
/// one, because with no naming convention to go on there is no way to tell
/// which is meant, and picking either would be a coin toss the user never
/// sees.
fn find_sole_onnx(dir: &Path) -> Result<PathBuf, InferError> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(InferError::Io)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.is_file() && path.extension().is_some_and(|ext| ext == "onnx"))
        .collect();
    found.sort();

    match found.len() {
        0 => Err(InferError::Other(format!(
            "no VITS .onnx model found in {}",
            dir.display()
        ))),
        1 => Ok(found.remove(0)),
        _ => Err(InferError::Other(format!(
            "{} holds {} .onnx files; a VITS bundle must hold exactly one",
            dir.display(),
            found.len()
        ))),
    }
}

/// Every `lexicon*.txt` in the bundle, sorted and comma-joined — the form
/// sherpa's `lexicon` field takes. Sorted so the same bundle always produces
/// the same string regardless of directory order.
fn joined_lexicons(dir: &Path) -> Option<String> {
    let mut paths: Vec<String> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("lexicon") && n.ends_with(".txt"))
        })
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    paths.sort();
    (!paths.is_empty()).then(|| paths.join(","))
}

/// Locates the files a bundle of this shape needs, or says which one is
/// missing. Pure path logic, so it is testable against a laid-out temp dir
/// without the native library.
pub fn find_tts_bundle(model_dir: &Path, kind: OnnxModelKind) -> Result<TtsBundle, InferError> {
    let tokens = require_file(model_dir, "tokens.txt")?;

    match kind {
        OnnxModelKind::TtsVits => {
            let data_dir = model_dir.join("espeak-ng-data");
            if !data_dir.is_dir() {
                return Err(InferError::Other(format!(
                    "missing espeak-ng-data in {}",
                    model_dir.display()
                )));
            }
            Ok(TtsBundle {
                model: find_sole_onnx(model_dir)?,
                tokens,
                data_dir: Some(data_dir),
                voices: None,
                dict_dir: None,
                lexicon: None,
            })
        }
        OnnxModelKind::TtsKokoro | OnnxModelKind::TtsKitten => Ok(TtsBundle {
            model: named_model(model_dir, kind)?,
            tokens,
            // espeak data is required by both in practice but is reported as
            // the missing piece it is rather than as a model-load failure.
            data_dir: Some(optional_dir(model_dir, "espeak-ng-data").ok_or_else(|| {
                InferError::Other(format!("missing espeak-ng-data in {}", model_dir.display()))
            })?),
            // The speaker table is what makes these multi-speaker at all, so
            // its absence is an error, not a missing extra.
            voices: Some(require_file(model_dir, "voices.bin")?),
            dict_dir: optional_dir(model_dir, "dict"),
            lexicon: joined_lexicons(model_dir),
        }),
        // Every non-voice shape, one arm: the reason none of them can be a
        // voice is the same, and an arm per family would have to be
        // remembered every time one is added.
        OnnxModelKind::Parakeet
        | OnnxModelKind::StreamingZipformer
        | OnnxModelKind::SpeakerSegmentation
        | OnnxModelKind::SpeakerEmbedding => {
            Err(InferError::Other(format!("{kind:?} is not a voice bundle")))
        }
    }
}

#[cfg(feature = "sherpa")]
pub struct SherpaOnnxTtsInference;

#[cfg(feature = "sherpa")]
impl SherpaOnnxTtsInference {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "sherpa")]
impl Default for SherpaOnnxTtsInference {
    fn default() -> Self {
        Self::new()
    }
}

/// Fills the one arm of `OfflineTtsModelConfig` this bundle belongs in.
/// Every other arm stays at its default, which is how sherpa is told which
/// family to load.
#[cfg(feature = "sherpa")]
fn model_config_for(
    bundle: &TtsBundle,
    kind: OnnxModelKind,
) -> Result<sherpa_onnx::OfflineTtsModelConfig, InferError> {
    use sherpa_onnx::{
        OfflineTtsKittenModelConfig, OfflineTtsKokoroModelConfig, OfflineTtsModelConfig,
        OfflineTtsVitsModelConfig,
    };

    let path = |p: &PathBuf| Some(p.to_string_lossy().into_owned());
    let maybe = |p: &Option<PathBuf>| p.as_ref().and_then(&path);

    let base = OfflineTtsModelConfig {
        num_threads: std::thread::available_parallelism()
            .map(|n| n.get() as i32)
            .unwrap_or(1),
        ..Default::default()
    };

    Ok(match kind {
        OnnxModelKind::TtsVits => OfflineTtsModelConfig {
            vits: OfflineTtsVitsModelConfig {
                model: path(&bundle.model),
                tokens: path(&bundle.tokens),
                data_dir: maybe(&bundle.data_dir),
                ..Default::default()
            },
            ..base
        },
        OnnxModelKind::TtsKokoro => OfflineTtsModelConfig {
            kokoro: OfflineTtsKokoroModelConfig {
                model: path(&bundle.model),
                voices: maybe(&bundle.voices),
                tokens: path(&bundle.tokens),
                data_dir: maybe(&bundle.data_dir),
                dict_dir: maybe(&bundle.dict_dir),
                lexicon: bundle.lexicon.clone(),
                ..Default::default()
            },
            ..base
        },
        OnnxModelKind::TtsKitten => OfflineTtsModelConfig {
            kitten: OfflineTtsKittenModelConfig {
                model: path(&bundle.model),
                voices: maybe(&bundle.voices),
                tokens: path(&bundle.tokens),
                data_dir: maybe(&bundle.data_dir),
                ..Default::default()
            },
            ..base
        },
        OnnxModelKind::Parakeet
        | OnnxModelKind::StreamingZipformer
        | OnnxModelKind::SpeakerSegmentation
        | OnnxModelKind::SpeakerEmbedding => {
            return Err(InferError::Other(format!("{kind:?} is not a voice bundle")))
        }
    })
}

#[cfg(feature = "sherpa")]
#[async_trait]
impl SherpaTtsInference for SherpaOnnxTtsInference {
    async fn synthesize(
        &self,
        text: &str,
        model_dir: &Path,
        kind: OnnxModelKind,
        opts: TtsSynthOpts,
    ) -> Result<AudioPcm, InferError> {
        let model_dir = model_dir.to_path_buf();
        let text = text.to_string();

        tokio::task::spawn_blocking(move || {
            use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig};

            let bundle = find_tts_bundle(&model_dir, kind)?;
            let config = OfflineTtsConfig {
                model: model_config_for(&bundle, kind)?,
                ..Default::default()
            };

            let tts = OfflineTts::create(&config)
                .ok_or_else(|| InferError::Other("failed to create sherpa OfflineTts".into()))?;

            let audio = tts
                .generate_with_config(
                    &text,
                    &GenerationConfig {
                        speed: opts.speed,
                        sid: opts.sid,
                        ..Default::default()
                    },
                    None::<fn(&[f32], f32) -> bool>,
                )
                .ok_or_else(|| InferError::Other("sherpa TTS returned no audio".into()))?;

            Ok(AudioPcm {
                samples: audio.samples().to_vec(),
                sample_rate_hz: audio.sample_rate() as u32,
            })
        })
        .await
        .map_err(|e| InferError::Other(format!("sherpa tts task join failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub struct FakeSherpaTtsInference;

    #[async_trait]
    impl SherpaTtsInference for FakeSherpaTtsInference {
        async fn synthesize(
            &self,
            text: &str,
            _model_dir: &Path,
            _kind: OnnxModelKind,
            _opts: TtsSynthOpts,
        ) -> Result<AudioPcm, InferError> {
            Ok(AudioPcm {
                samples: vec![0.0; text.len() * 100],
                sample_rate_hz: 22_050,
            })
        }
    }

    #[tokio::test]
    async fn fake_tts_returns_pcm() {
        let inference = FakeSherpaTtsInference;
        let pcm = inference
            .synthesize(
                "hello",
                Path::new("/tmp/tts-model"),
                OnnxModelKind::TtsVits,
                TtsSynthOpts::default(),
            )
            .await
            .unwrap();
        assert_eq!(pcm.sample_rate_hz, 22_050);
        assert_eq!(pcm.samples.len(), 500);
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"x").unwrap();
    }

    /// A Kokoro bundle as the release ships it.
    fn lay_out_kokoro(dir: &Path, multilingual: bool) {
        touch(&dir.join("model.int8.onnx"));
        touch(&dir.join("voices.bin"));
        touch(&dir.join("tokens.txt"));
        touch(&dir.join("espeak-ng-data").join("phontab"));
        if multilingual {
            touch(&dir.join("dict").join("jieba.dict.utf8"));
            touch(&dir.join("lexicon-us-en.txt"));
            touch(&dir.join("lexicon-zh.txt"));
        }
    }

    #[test]
    fn finds_every_file_in_an_english_kokoro_bundle() {
        let dir = tempfile::tempdir().unwrap();
        lay_out_kokoro(dir.path(), false);

        let bundle = find_tts_bundle(dir.path(), OnnxModelKind::TtsKokoro).unwrap();
        assert_eq!(bundle.model, dir.path().join("model.int8.onnx"));
        assert_eq!(bundle.tokens, dir.path().join("tokens.txt"));
        assert_eq!(bundle.voices, Some(dir.path().join("voices.bin")));
        assert_eq!(bundle.data_dir, Some(dir.path().join("espeak-ng-data")));
        // The English bundle has neither, and their absence is not an error.
        assert_eq!(bundle.dict_dir, None);
        assert_eq!(bundle.lexicon, None);
    }

    #[test]
    fn the_multilingual_kokoro_bundle_carries_its_dict_and_both_lexicons() {
        let dir = tempfile::tempdir().unwrap();
        lay_out_kokoro(dir.path(), true);

        let bundle = find_tts_bundle(dir.path(), OnnxModelKind::TtsKokoro).unwrap();
        assert_eq!(bundle.dict_dir, Some(dir.path().join("dict")));
        // sherpa takes several lexicons as one comma-joined string, in a
        // stable order.
        let lexicon = bundle.lexicon.unwrap();
        assert_eq!(
            lexicon,
            format!(
                "{},{}",
                dir.path().join("lexicon-us-en.txt").display(),
                dir.path().join("lexicon-zh.txt").display()
            )
        );
    }

    #[test]
    fn a_kokoro_bundle_missing_a_required_file_says_which_one() {
        for missing in ["model.int8.onnx", "voices.bin", "tokens.txt"] {
            let dir = tempfile::tempdir().unwrap();
            lay_out_kokoro(dir.path(), false);
            std::fs::remove_file(dir.path().join(missing)).unwrap();

            let err = find_tts_bundle(dir.path(), OnnxModelKind::TtsKokoro)
                .expect_err("a bundle missing {missing} must not load");
            assert!(
                err.to_string().contains(missing) || err.to_string().contains("model"),
                "unhelpful error for a missing {missing}: {err}"
            );
        }

        let dir = tempfile::tempdir().unwrap();
        lay_out_kokoro(dir.path(), false);
        std::fs::remove_dir_all(dir.path().join("espeak-ng-data")).unwrap();
        let err = find_tts_bundle(dir.path(), OnnxModelKind::TtsKokoro).unwrap_err();
        assert!(err.to_string().contains("espeak-ng-data"), "{err}");
    }

    #[test]
    fn kitten_prefers_its_fp16_weights() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("model.fp16.onnx"));
        touch(&dir.path().join("model.int8.onnx"));
        touch(&dir.path().join("voices.bin"));
        touch(&dir.path().join("tokens.txt"));
        touch(&dir.path().join("espeak-ng-data").join("phontab"));

        let bundle = find_tts_bundle(dir.path(), OnnxModelKind::TtsKitten).unwrap();
        assert_eq!(bundle.model, dir.path().join("model.fp16.onnx"));
    }

    #[test]
    fn a_piper_bundle_loads_by_its_one_onnx_file() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("en_US-lessac-medium.onnx"));
        touch(&dir.path().join("en_US-lessac-medium.onnx.json"));
        touch(&dir.path().join("tokens.txt"));
        touch(&dir.path().join("espeak-ng-data").join("phontab"));

        let bundle = find_tts_bundle(dir.path(), OnnxModelKind::TtsVits).unwrap();
        assert_eq!(bundle.model, dir.path().join("en_US-lessac-medium.onnx"));
        assert_eq!(bundle.voices, None);
    }

    /// The bug the old heuristic hid: with two models in one directory it
    /// took whichever the filesystem listed first and synthesized in the
    /// wrong voice, silently. Refusing is the only honest answer — a Piper
    /// bundle has no naming convention to pick by.
    #[test]
    fn two_onnx_files_in_a_piper_bundle_are_refused_rather_than_guessed() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("en_US-lessac-medium.onnx"));
        touch(&dir.path().join("en_US-amy-low.onnx"));
        touch(&dir.path().join("tokens.txt"));
        touch(&dir.path().join("espeak-ng-data").join("phontab"));

        let err = find_tts_bundle(dir.path(), OnnxModelKind::TtsVits).unwrap_err();
        assert!(err.to_string().contains("exactly one"), "{err}");
    }

    #[test]
    fn a_kokoro_bundle_is_not_loadable_as_vits() {
        let dir = tempfile::tempdir().unwrap();
        lay_out_kokoro(dir.path(), false);
        // Two Kokoro quantizations in one directory is exactly the case the
        // old "first .onnx that is not an encoder" rule got wrong.
        touch(&dir.path().join("model.onnx"));
        assert!(find_tts_bundle(dir.path(), OnnxModelKind::TtsVits).is_err());
        // Addressed by kind, it still resolves — to the preferred weights.
        let bundle = find_tts_bundle(dir.path(), OnnxModelKind::TtsKokoro).unwrap();
        assert_eq!(bundle.model, dir.path().join("model.int8.onnx"));
    }

    /// Every non-voice family is refused by name, including the diarization
    /// pair added with `ModelKind::Diarization` — a new shape that silently
    /// fell through to a voice loader would be a model-load failure with no
    /// clue in it.
    #[test]
    fn a_bundle_that_is_not_a_voice_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("tokens.txt"));
        for kind in [
            OnnxModelKind::Parakeet,
            OnnxModelKind::StreamingZipformer,
            OnnxModelKind::SpeakerSegmentation,
            OnnxModelKind::SpeakerEmbedding,
        ] {
            let err = find_tts_bundle(dir.path(), kind).unwrap_err();
            let message = err.to_string();
            assert!(message.contains("not a voice"), "{message}");
            assert!(message.contains(&format!("{kind:?}")), "{message}");
        }
    }
}
