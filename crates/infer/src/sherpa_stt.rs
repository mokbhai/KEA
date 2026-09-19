//! Offline recognition through sherpa-onnx, and the decoder biasing that goes
//! with it.
//!
//! ## What "hotwords" cost, and when they are worth paying
//!
//! sherpa only consults `hotwords_file` under `modified_beam_search`; under
//! the greedy decoder the field is read and ignored. So biasing is never free:
//! it swaps a one-best decode for a beam search over `max_active_paths`
//! hypotheses. An empty term list must therefore leave the decoder alone
//! rather than "turn the feature on and pass nothing", which is why
//! [`plan_hotwords`] answers greedy for an empty list.
//!
//! The second constraint is encoding. sherpa maps each hotword into the
//! model's own modelling units before building the context graph. For a
//! subword (BPE) model that needs the sentencepiece vocabulary the units came
//! from — sherpa's `bpe_vocab` — and without it the terms are looked up whole
//! in `tokens.txt`, fail, and are skipped one by one with a warning from the
//! native library. Verified against the shipped 1.13.3 static library: an
//! unencodable hotword is skipped ("Some hotwords failed to encode and were
//! skipped"), not fatal — but it is also not honoured, and paying for a beam
//! search to get nothing is worse than not biasing at all. So the plan below
//! requires a BPE vocabulary in the bundle.
//!
//! NOTE, measured: the Parakeet TDT bundles in the catalog ship
//! `encoder/decoder/joiner.int8.onnx`, `tokens.txt` and `test_wavs/` — and no
//! `bpe.vocab`. On those bundles as published, [`plan_hotwords`] answers
//! greedy and says so in the log. No accuracy claim is made here either way:
//! the benefit was never measured, and nothing in this module pretends it was.

use std::path::Path;
#[cfg(feature = "sherpa")]
use std::path::PathBuf;

use async_trait::async_trait;

use crate::error::InferError;
use crate::registry::OnnxModelKind;
#[cfg(feature = "sherpa")]
use crate::types::{group_tokens_into_segments, TOKEN_GROUP_GAP_MS, TOKEN_GROUP_MAX_CUE_MS};
use crate::types::{AudioPcm, SttResult};

/// How strongly a matched hotword is boosted. sherpa's own CLI default; the
/// scale is "bonus per token of the phrase", so a much larger value starts
/// inventing the term out of silence.
pub const HOTWORDS_SCORE: f32 = 1.5;

/// The sentencepiece vocabulary sherpa needs to encode a hotword into the
/// subword units a transducer actually decodes in. Two columns — token and
/// log probability — which is why `tokens.txt` cannot stand in for it.
const BPE_VOCAB_FILENAMES: &[&str] = &["bpe.vocab", "bpe.model"];

/// Canonical spellings to bias decoding toward, normalized once.
///
/// A type rather than a bare `&[String]` so the normalization — trim, drop
/// empties, drop duplicates, drop anything with a newline in it — happens in
/// one place. The newline rule is not cosmetic: sherpa's hotwords file is one
/// phrase per line, so a term containing a line break would silently become
/// two different hotwords.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SttHotwords {
    terms: Vec<String>,
}

impl SttHotwords {
    pub fn new<I, S>(terms: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut out: Vec<String> = Vec::new();
        for term in terms {
            let term = term.as_ref().trim();
            if term.is_empty() || term.contains(['\n', '\r']) {
                continue;
            }
            if !out.iter().any(|existing| existing == term) {
                out.push(term.to_string());
            }
        }
        Self { terms: out }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    pub fn terms(&self) -> &[String] {
        &self.terms
    }

    /// The body of the file sherpa's `hotwords_file` points at: one phrase per
    /// line, trailing newline included so the last line is a line.
    pub fn file_body(&self) -> String {
        let mut body = self.terms.join("\n");
        if !body.is_empty() {
            body.push('\n');
        }
        body
    }
}

/// What one decode does about hotwords: the decoding method to run, and the
/// files to run it with.
///
/// Decided in one place, outside the `sherpa` feature gate, because the
/// decision is pure path-and-list logic and because the alternative — a chain
/// of `if`s inside the blocking task — is where a parameter quietly stops
/// being honoured. See the module note for why an empty list has to mean
/// "change nothing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotwordPlan {
    /// `greedy_search` unless hotwords are actually in play; sherpa reads
    /// `hotwords_file` only under `modified_beam_search`.
    pub decoding_method: &'static str,
    /// The sentencepiece vocabulary to encode the terms with, and the file
    /// body to write. `None` means no biasing at all.
    pub hotwords: Option<HotwordFiles>,
}

/// The two things a biased decode needs beyond the model itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotwordFiles {
    /// sherpa's `bpe_vocab`, found in the bundle.
    pub bpe_vocab: std::path::PathBuf,
    /// What to write into the file `hotwords_file` will point at.
    pub body: String,
}

/// Whether sherpa can build a context graph for a bundle of this shape.
///
/// Only the transducer can. Moonshine is an encoder/decoder model with no
/// context-graph hook in `OfflineMoonshineModelConfig`, so
/// `modified_beam_search` there buys a slower decode and biases nothing —
/// which is the same trade [`plan_hotwords`] already refuses for a bundle
/// with no sentencepiece vocabulary.
fn supports_hotwords(kind: OnnxModelKind) -> bool {
    matches!(kind, OnnxModelKind::Parakeet)
}

/// Decides whether this bundle can honour these terms, and how.
///
/// Answering "greedy, no files" is a real answer, not a failure: it is what
/// keeps a user with no vocabulary — and a user whose model cannot encode one
/// — from paying for a beam search that buys nothing.
pub fn plan_hotwords(model_dir: &Path, kind: OnnxModelKind, hotwords: &SttHotwords) -> HotwordPlan {
    const GREEDY: &str = "greedy_search";
    const BEAM: &str = "modified_beam_search";

    if hotwords.is_empty() {
        return HotwordPlan {
            decoding_method: GREEDY,
            hotwords: None,
        };
    }

    if !supports_hotwords(kind) {
        // Same reasoning as the missing-vocabulary case below: the transcript
        // is still correct and `apply_vocabulary` still fixes the spelling
        // afterwards, so this is a log line rather than an error — but it
        // must not be silence about a setting the user can see.
        tracing::debug!(
            ?kind,
            terms = hotwords.terms().len(),
            "this model family has no decoder biasing; vocabulary terms are \
             still applied to the transcript"
        );
        return HotwordPlan {
            decoding_method: GREEDY,
            hotwords: None,
        };
    }

    let Some(bpe_vocab) = BPE_VOCAB_FILENAMES
        .iter()
        .map(|name| model_dir.join(name))
        .find(|path| path.is_file())
    else {
        // Deliberately loud, and deliberately not an error: the transcript is
        // still correct, and the replacement pass
        // (`kea_core::dictation::apply_vocabulary`) still fixes the spelling
        // afterwards. What must not happen is silence about a setting the
        // user can see in the UI and this decode cannot use.
        tracing::warn!(
            model_dir = %model_dir.display(),
            terms = hotwords.terms().len(),
            expected = ?BPE_VOCAB_FILENAMES,
            "this model ships no sentencepiece vocabulary, so decoder biasing \
             is skipped; vocabulary terms are still applied to the transcript"
        );
        return HotwordPlan {
            decoding_method: GREEDY,
            hotwords: None,
        };
    };

    HotwordPlan {
        decoding_method: BEAM,
        hotwords: Some(HotwordFiles {
            bpe_vocab,
            body: hotwords.file_body(),
        }),
    }
}

#[async_trait]
pub trait SherpaSttInference: Send + Sync {
    /// Transcribes mono PCM with the ONNX bundle in `model_dir`, biased toward
    /// `hotwords`.
    ///
    /// There is deliberately no language parameter. The NeMo transducer this
    /// drives has no language setting — sherpa's `OfflineTransducerModelConfig`
    /// carries only the encoder/decoder/joiner paths — so a language argument
    /// could only ever be accepted and dropped, which is what this signature
    /// used to do. A parameter that looks honoured all the way down from the
    /// STT setting is worse than one that was never plumbed.
    ///
    /// `hotwords` is here under exactly that rule, not against it. It reaches
    /// `OfflineRecognizerConfig::hotwords_file` together with the
    /// `modified_beam_search` the field requires — or, when the bundle cannot
    /// encode the terms, it changes nothing and says so in the log rather than
    /// pretending. [`plan_hotwords`] is that decision, and it is testable
    /// without a model on disk.
    ///
    /// `kind` says which sherpa model config the bundle in `model_dir` has to
    /// be loaded through — a transducer triple or a Moonshine quartet. A
    /// parameter rather than something sniffed from the directory, because
    /// the catalog already knows it and guessing from the files present is
    /// the heuristic `sherpa_tts` was rewritten to remove.
    async fn transcribe(
        &self,
        pcm: AudioPcm,
        model_dir: &Path,
        kind: OnnxModelKind,
        hotwords: &SttHotwords,
    ) -> Result<SttResult, InferError>;
}

#[cfg(feature = "sherpa")]
pub struct SherpaOnnxSttInference;

#[cfg(feature = "sherpa")]
impl SherpaOnnxSttInference {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "sherpa")]
impl Default for SherpaOnnxSttInference {
    fn default() -> Self {
        Self::new()
    }
}

/// The files one decode loads, already sorted into the sherpa config they
/// belong to.
///
/// An enum rather than a tuple that grows a fourth path: a Moonshine bundle's
/// four files are not "a transducer plus one more", they go into a different
/// `OfflineModelConfig` field entirely, and a tuple would let the wrong four
/// paths reach the wrong config with nothing in the type to stop it.
#[cfg(feature = "sherpa")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SttModelFiles {
    /// NeMo transducer: encoder, decoder, joiner.
    Transducer {
        encoder: PathBuf,
        decoder: PathBuf,
        joiner: PathBuf,
        tokens: PathBuf,
    },
    /// Moonshine v1: preprocessor, encoder, uncached decoder, cached decoder.
    Moonshine {
        preprocessor: PathBuf,
        encoder: PathBuf,
        uncached_decoder: PathBuf,
        cached_decoder: PathBuf,
        tokens: PathBuf,
    },
}

/// Locates the files for one bundle, dispatching on the catalog's own shape.
#[cfg(feature = "sherpa")]
pub fn find_stt_model_files(
    model_dir: &Path,
    kind: OnnxModelKind,
) -> Result<SttModelFiles, InferError> {
    let tokens = model_dir.join("tokens.txt");
    if !tokens.is_file() {
        return Err(InferError::Other(format!(
            "missing tokens.txt in {}",
            model_dir.display()
        )));
    }

    match kind {
        OnnxModelKind::Parakeet => Ok(SttModelFiles::Transducer {
            encoder: find_first_existing(model_dir, &["encoder.int8.onnx", "encoder.onnx"])?,
            decoder: find_first_existing(model_dir, &["decoder.int8.onnx", "decoder.onnx"])?,
            joiner: find_first_existing(model_dir, &["joiner.int8.onnx", "joiner.onnx"])?,
            tokens,
        }),
        // Explicit per-role filename lists, best first, the way
        // `sherpa_tts::model_filenames` does it. A "first .onnx in the
        // directory" rule cannot work here at all: there are four of them and
        // each one goes in a different field, so picking by directory order
        // would load the cached decoder as the encoder.
        //
        // The quantized bundles in the catalog ship the `.int8` spellings;
        // the float alternatives are listed so a user who unpacked the
        // non-quantized release into the same slot still loads.
        OnnxModelKind::Moonshine => Ok(SttModelFiles::Moonshine {
            preprocessor: find_first_existing(
                model_dir,
                &["preprocess.onnx", "preprocess.int8.onnx"],
            )?,
            encoder: find_first_existing(model_dir, &["encode.int8.onnx", "encode.onnx"])?,
            uncached_decoder: find_first_existing(
                model_dir,
                &["uncached_decode.int8.onnx", "uncached_decode.onnx"],
            )?,
            cached_decoder: find_first_existing(
                model_dir,
                &["cached_decode.int8.onnx", "cached_decode.onnx"],
            )?,
            tokens,
        }),
        other => Err(InferError::Other(format!(
            "{other:?} is not an offline recognizer bundle"
        ))),
    }
}

#[cfg(feature = "sherpa")]
fn find_first_existing(dir: &Path, names: &[&str]) -> Result<PathBuf, InferError> {
    for name in names {
        let path = dir.join(name);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(InferError::Other(format!(
        "none of {names:?} found in {}",
        dir.display()
    )))
}

/// Builds the recognizer config for one decode.
///
/// Factored out of `transcribe` so a test can assert that a plan carrying
/// hotwords arrives in the config as `hotwords_file`, `modified_beam_search`
/// and a `bpe` modelling unit together, rather than being accepted and
/// dropped on the way. `hotwords_file` is a path the caller has already
/// written: sherpa aborts the process if the file named there does not exist.
#[cfg(feature = "sherpa")]
fn recognizer_config(
    files: SttModelFiles,
    plan: &HotwordPlan,
    hotwords_file: Option<&Path>,
) -> sherpa_onnx::OfflineRecognizerConfig {
    use sherpa_onnx::{
        OfflineMoonshineModelConfig, OfflineRecognizerConfig, OfflineTransducerModelConfig,
    };

    let text = |p: &Path| p.to_string_lossy().into_owned();

    let mut config = OfflineRecognizerConfig {
        decoding_method: Some(plan.decoding_method.to_string()),
        ..OfflineRecognizerConfig::default()
    };
    let tokens = match files {
        SttModelFiles::Transducer {
            encoder,
            decoder,
            joiner,
            tokens,
        } => {
            config.model_config.transducer = OfflineTransducerModelConfig {
                encoder: Some(text(&encoder)),
                decoder: Some(text(&decoder)),
                joiner: Some(text(&joiner)),
            };
            // sherpa cannot tell a NeMo transducer from an icefall one by
            // looking at the weights; the tag is what selects the right
            // blank/feature handling.
            config.model_config.model_type = Some("nemo_transducer".into());
            tokens
        }
        SttModelFiles::Moonshine {
            preprocessor,
            encoder,
            uncached_decoder,
            cached_decoder,
            tokens,
        } => {
            config.model_config.moonshine = OfflineMoonshineModelConfig {
                preprocessor: Some(text(&preprocessor)),
                encoder: Some(text(&encoder)),
                uncached_decoder: Some(text(&uncached_decoder)),
                cached_decoder: Some(text(&cached_decoder)),
                // v2's single merged decoder; the v1 bundles in the catalog
                // ship the pair above instead.
                merged_decoder: None,
            };
            // No `model_type` here on purpose: sherpa infers Moonshine from
            // the populated config, and naming a type it does not know is how
            // the native library ends up calling `exit`.
            tokens
        }
    };
    config.model_config.tokens = Some(text(&tokens));
    config.model_config.num_threads = std::thread::available_parallelism()
        .map(|n| n.get() as i32)
        .unwrap_or(1);

    // Both halves or neither: sherpa's default modelling unit is `cjkchar`,
    // which looks an English phrase up whole in `tokens.txt` and skips it, so
    // naming the vocabulary without naming the unit would be biasing that
    // never fires.
    if let (Some(hotwords), Some(path)) = (plan.hotwords.as_ref(), hotwords_file) {
        config.model_config.modeling_unit = Some("bpe".into());
        config.model_config.bpe_vocab = Some(text(&hotwords.bpe_vocab));
        config.hotwords_file = Some(text(path));
        config.hotwords_score = HOTWORDS_SCORE;
    }

    config
}

#[cfg(feature = "sherpa")]
#[async_trait]
impl SherpaSttInference for SherpaOnnxSttInference {
    async fn transcribe(
        &self,
        pcm: AudioPcm,
        model_dir: &Path,
        kind: OnnxModelKind,
        hotwords: &SttHotwords,
    ) -> Result<SttResult, InferError> {
        let model_dir = model_dir.to_path_buf();
        let hotwords = hotwords.clone();
        let samples = pcm.samples;
        let sample_rate = pcm.sample_rate_hz;

        tokio::task::spawn_blocking(move || {
            use sherpa_onnx::OfflineRecognizer;

            let files = find_stt_model_files(&model_dir, kind)?;
            let plan = plan_hotwords(&model_dir, kind, &hotwords);

            // The file has to exist for as long as the recognizer is being
            // created — sherpa reads it there, and calls `exit` if it cannot
            // be opened. The guard therefore stays alive past `create`; the
            // terms are the user's vocabulary, so it is a temp file rather
            // than something left beside the model.
            let hotwords_file = match plan.hotwords.as_ref() {
                Some(files) => {
                    let temp = tempfile::NamedTempFile::new()?;
                    std::fs::write(temp.path(), files.body.as_bytes())?;
                    Some(temp)
                }
                None => None,
            };
            let config =
                recognizer_config(files, &plan, hotwords_file.as_ref().map(|temp| temp.path()));

            let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
                InferError::Other("failed to create sherpa OfflineRecognizer".into())
            })?;
            drop(hotwords_file);

            let stream = recognizer.create_stream();
            stream.accept_waveform(sample_rate as i32, &samples);
            recognizer.decode(&stream);

            let result = stream
                .get_result()
                .ok_or_else(|| InferError::Other(format!("sherpa {kind:?} returned no result")))?;

            // sherpa reports one timestamp per *token*, so these have to be
            // grouped into cues before they are readable as subtitles — one
            // cue per word is not a subtitle. A model or build that reports
            // none leaves `segments` empty rather than inventing a span.
            let segments = match result.timestamps.as_deref() {
                Some(starts) => group_tokens_into_segments(
                    &result.tokens,
                    starts,
                    result.durations.as_deref(),
                    TOKEN_GROUP_GAP_MS,
                    TOKEN_GROUP_MAX_CUE_MS,
                ),
                None => Vec::new(),
            };

            Ok(SttResult {
                text: result.text,
                segments,
            })
        })
        .await
        .map_err(|e| InferError::Other(format!("sherpa stt task join failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub struct FakeSherpaSttInference;

    #[async_trait]
    impl SherpaSttInference for FakeSherpaSttInference {
        async fn transcribe(
            &self,
            pcm: AudioPcm,
            _model_dir: &Path,
            kind: OnnxModelKind,
            _hotwords: &SttHotwords,
        ) -> Result<SttResult, InferError> {
            Ok(SttResult::text_only(format!(
                "{kind:?}: {} samples",
                pcm.samples.len()
            )))
        }
    }

    #[test]
    fn fake_sherpa_stt_trait_is_usable() {
        let _ = std::any::type_name::<FakeSherpaSttInference>();
    }

    #[tokio::test]
    async fn fake_inference_returns_sample_count() {
        let inference = FakeSherpaSttInference;
        let out = inference
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 1600],
                    sample_rate_hz: 16_000,
                },
                Path::new("/tmp/parakeet-model"),
                OnnxModelKind::Parakeet,
                &SttHotwords::default(),
            )
            .await
            .unwrap();
        assert!(out.text.contains("Parakeet"));
        assert!(out.text.contains("1600"));
    }

    /// The file sherpa reads is one phrase per line, so anything that could
    /// split or merge a line has to be handled before it is written.
    #[test]
    fn hotwords_are_normalized_once_on_the_way_in() {
        let hotwords = SttHotwords::new([
            "  KittyClaw ",
            "KEA",
            // A duplicate after trimming, and an empty entry.
            "KittyClaw",
            "   ",
            // A term with a line break would become two hotwords.
            "two\nlines",
        ]);
        assert_eq!(hotwords.terms(), ["KittyClaw", "KEA"]);
        assert_eq!(hotwords.file_body(), "KittyClaw\nKEA\n");

        assert!(SttHotwords::default().is_empty());
        assert_eq!(SttHotwords::new(Vec::<String>::new()).file_body(), "");
    }

    fn touch(path: &Path) {
        std::fs::write(path, b"x").unwrap();
    }

    /// An empty list must leave the decoder exactly as it was: hotwords only
    /// work under `modified_beam_search`, and that is a real cost to pay for
    /// a user who has no vocabulary at all.
    #[test]
    fn no_terms_means_no_beam_search() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("bpe.vocab"));

        let plan = plan_hotwords(dir.path(), OnnxModelKind::Parakeet, &SttHotwords::default());
        assert_eq!(plan.decoding_method, "greedy_search");
        assert_eq!(plan.hotwords, None);
    }

    /// With terms *and* a vocabulary to encode them with, the plan switches
    /// the decoder and carries both files.
    #[test]
    fn terms_plus_a_bpe_vocabulary_switch_the_decoder() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("bpe.vocab"));

        let plan = plan_hotwords(
            dir.path(),
            OnnxModelKind::Parakeet,
            &SttHotwords::new(["KittyClaw", "KEA"]),
        );
        assert_eq!(plan.decoding_method, "modified_beam_search");
        let files = plan.hotwords.expect("a plan with terms carries files");
        assert_eq!(files.bpe_vocab, dir.path().join("bpe.vocab"));
        assert_eq!(files.body, "KittyClaw\nKEA\n");
    }

    /// The measured case for the shipped Parakeet bundles: no sentencepiece
    /// vocabulary, so sherpa would skip every term one by one *after* the
    /// beam search had already been paid for. Staying greedy is the honest
    /// answer, and the warning is where the user can find out why.
    #[test]
    fn a_bundle_with_no_sentencepiece_vocabulary_stays_greedy() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("tokens.txt"));
        touch(&dir.path().join("encoder.int8.onnx"));

        let plan = plan_hotwords(
            dir.path(),
            OnnxModelKind::Parakeet,
            &SttHotwords::new(["KittyClaw"]),
        );
        assert_eq!(plan.decoding_method, "greedy_search");
        assert_eq!(plan.hotwords, None);
    }

    /// Moonshine has no context graph to put hotwords into, so the decoder
    /// must stay greedy even with terms *and* a vocabulary file present —
    /// otherwise the user pays for a beam search that biases nothing.
    #[test]
    fn moonshine_never_switches_to_beam_search() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("bpe.vocab"));

        let plan = plan_hotwords(
            dir.path(),
            OnnxModelKind::Moonshine,
            &SttHotwords::new(["KittyClaw"]),
        );
        assert_eq!(plan.decoding_method, "greedy_search");
        assert_eq!(plan.hotwords, None);
    }

    /// A Moonshine bundle is four files with four distinct roles. Each one
    /// has to be found by *name*: the failure this guards against is loading
    /// the cached decoder into the encoder slot, which no error would report.
    #[cfg(feature = "sherpa")]
    #[test]
    fn a_moonshine_bundle_is_located_file_by_file() {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "tokens.txt",
            "preprocess.onnx",
            "encode.int8.onnx",
            "uncached_decode.int8.onnx",
            "cached_decode.int8.onnx",
        ] {
            touch(&dir.path().join(name));
        }

        let files = find_stt_model_files(dir.path(), OnnxModelKind::Moonshine).unwrap();
        let SttModelFiles::Moonshine {
            preprocessor,
            encoder,
            uncached_decoder,
            cached_decoder,
            tokens,
        } = files.clone()
        else {
            panic!("a moonshine bundle must locate as moonshine");
        };
        assert!(preprocessor.ends_with("preprocess.onnx"));
        assert!(encoder.ends_with("encode.int8.onnx"));
        assert!(uncached_decoder.ends_with("uncached_decode.int8.onnx"));
        assert!(cached_decoder.ends_with("cached_decode.int8.onnx"));
        assert!(tokens.ends_with("tokens.txt"));

        // And the paths have to land in the moonshine config, not the
        // transducer one sitting beside it in the same struct.
        let plan = plan_hotwords(
            dir.path(),
            OnnxModelKind::Moonshine,
            &SttHotwords::default(),
        );
        let config = recognizer_config(files, &plan, None);
        assert_eq!(
            config.model_config.moonshine.encoder,
            Some(
                dir.path()
                    .join("encode.int8.onnx")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        assert_eq!(config.model_config.transducer.encoder, None);
        // sherpa infers the family from the filled config; a `model_type` it
        // does not recognize makes the native library exit the process.
        assert_eq!(config.model_config.model_type, None);
        assert_eq!(config.decoding_method.as_deref(), Some("greedy_search"));
    }

    /// An incomplete bundle names the files it wanted rather than failing
    /// somewhere inside the native library.
    #[cfg(feature = "sherpa")]
    #[test]
    fn a_half_unpacked_moonshine_bundle_is_refused_by_name() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("tokens.txt"));
        touch(&dir.path().join("preprocess.onnx"));
        let err = find_stt_model_files(dir.path(), OnnxModelKind::Moonshine).unwrap_err();
        assert!(err.to_string().contains("encode"), "{err}");
    }

    /// `bpe.model` is the other name the same vocabulary ships under.
    #[test]
    fn either_published_vocabulary_filename_is_accepted() {
        for name in ["bpe.vocab", "bpe.model"] {
            let dir = tempfile::tempdir().unwrap();
            touch(&dir.path().join(name));
            let plan = plan_hotwords(
                dir.path(),
                OnnxModelKind::Parakeet,
                &SttHotwords::new(["KEA"]),
            );
            let files = plan.hotwords.expect("{name} should be accepted");
            assert_eq!(files.bpe_vocab, dir.path().join(name));
        }
    }

    /// The point of the whole exercise: the terms must reach the recognizer
    /// config, not merely be accepted by the signature. This asserts against
    /// the real `OfflineRecognizerConfig`, so it only builds where sherpa
    /// does.
    #[cfg(feature = "sherpa")]
    #[test]
    fn the_terms_reach_the_recognizer_config() {
        let dir = tempfile::tempdir().unwrap();
        touch(&dir.path().join("bpe.vocab"));
        touch(&dir.path().join("tokens.txt"));
        touch(&dir.path().join("encoder.int8.onnx"));
        touch(&dir.path().join("decoder.int8.onnx"));
        touch(&dir.path().join("joiner.int8.onnx"));
        let files = find_stt_model_files(dir.path(), OnnxModelKind::Parakeet).unwrap();

        let plan = plan_hotwords(
            dir.path(),
            OnnxModelKind::Parakeet,
            &SttHotwords::new(["KittyClaw"]),
        );
        let hotwords_path = dir.path().join("hotwords.txt");
        std::fs::write(
            &hotwords_path,
            plan.hotwords.as_ref().unwrap().body.as_bytes(),
        )
        .unwrap();

        let config = recognizer_config(files.clone(), &plan, Some(&hotwords_path));
        assert_eq!(
            config.hotwords_file,
            Some(hotwords_path.to_string_lossy().into_owned())
        );
        assert_eq!(config.hotwords_score, HOTWORDS_SCORE);
        // sherpa reads `hotwords_file` only under this decoder.
        assert_eq!(
            config.decoding_method.as_deref(),
            Some("modified_beam_search")
        );
        // And it can only encode the terms with the model's own subword units.
        assert_eq!(config.model_config.modeling_unit.as_deref(), Some("bpe"));
        assert_eq!(
            config.model_config.bpe_vocab,
            Some(dir.path().join("bpe.vocab").to_string_lossy().into_owned())
        );

        // With no terms, none of it is touched — and the greedy decoder the
        // engine has always used is what runs.
        let plain = plan_hotwords(dir.path(), OnnxModelKind::Parakeet, &SttHotwords::default());
        let config = recognizer_config(files, &plain, None);
        assert_eq!(config.hotwords_file, None);
        assert_eq!(config.hotwords_score, 0.0);
        assert_eq!(config.decoding_method.as_deref(), Some("greedy_search"));
        assert_eq!(config.model_config.modeling_unit, None);
        assert_eq!(config.model_config.bpe_vocab, None);
    }
}
