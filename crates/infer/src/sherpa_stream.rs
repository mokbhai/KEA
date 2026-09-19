//! Streaming (online) speech recognition for live partial transcripts.
//!
//! Deliberately **synchronous**, unlike [`crate::sherpa_stt::SherpaSttInference`].
//! That trait puts its `spawn_blocking` inside infer, which is right for one
//! call per utterance and wrong for one call per 10 ms frame: the dispatch and
//! thread-pool churn would cost more than the decode. Keeping this layer
//! blocking lets the engines layer own exactly one long-lived blocking task per
//! session and drive it from there.
//!
//! What comes out of here is **display only**. The offline engine re-decodes
//! the complete buffer when the user stops, and that is what gets inserted —
//! see `crates/features/src/dictation.rs`. This pass is allowed to be lossy, to
//! fall behind, to be wrong, and not to exist at all.

use std::path::{Path, PathBuf};

use crate::error::InferError;
use crate::types::{AudioPcm, StreamingCfg};

/// Opens streaming recognition sessions against an installed model bundle.
pub trait SherpaStreamingInference: Send + Sync {
    fn open(
        &self,
        model_dir: &Path,
        cfg: StreamingCfg,
    ) -> Result<Box<dyn SherpaStreamSession>, InferError>;
}

/// One live recognition session. `&mut self` on [`SherpaStreamSession::accept`]
/// is load bearing: sherpa's `unsafe impl Send/Sync` for `OnlineRecognizer` and
/// `OnlineStream` is justified in-source as "thread-safe for single-object
/// usage", which is a claim about data races, not about two threads
/// interleaving `decode` on one stream. Exclusive access makes the interleaving
/// unrepresentable.
pub trait SherpaStreamSession: Send {
    /// Feed one chunk of mono PCM. `Ok(None)` means the hypothesis did not
    /// change, which is the common case for a greedy decoder.
    fn accept(&mut self, pcm: AudioPcm) -> Result<Option<String>, InferError>;

    /// Whether the endpointing rules considered the utterance finished at the
    /// last [`accept`](Self::accept).
    ///
    /// Read *after* `accept` rather than folded into its return value so the
    /// trait stays usable by a backend with no endpointer at all, which simply
    /// answers `false` forever.
    fn endpointed(&self) -> bool;

    /// Flush trailing context and return the final hypothesis for the session.
    fn finish(&mut self) -> Result<String, InferError>;
}

/// The four files a streaming transducer bundle is loaded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamingModelFiles {
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub joiner: PathBuf,
    pub tokens: PathBuf,
}

/// Which of a role's candidate files to prefer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Quantization {
    /// The encoder and the joiner: int8 is where the size and the time are,
    /// and it is what sherpa's own model cards run.
    PreferInt8,
    /// The decoder: a small embedding-plus-convolution stack where int8 saves
    /// almost nothing and is the one place quantization is reported to cost
    /// accuracy. sherpa's streaming-zipformer examples pair an int8 encoder and
    /// joiner with an fp32 decoder for exactly this reason.
    PreferFloat,
}

/// Locates the ONNX files in a streaming Zipformer bundle.
///
/// [`crate::sherpa_stt`]'s `find_parakeet_model_files` cannot serve: it asks
/// for the literal names `encoder.onnx` / `encoder.int8.onnx`, and a streaming
/// bundle names its files with the training epoch and averaging window baked in
/// (`encoder-epoch-99-avg-1.int8.onnx`). So this matches by *role prefix* and
/// `.onnx` suffix instead, which also means a bundle that renames its epoch
/// does not need a code change.
///
/// Errors carry the directory listing, because silently picking the wrong file
/// — a joiner loaded as a decoder, an fp32 file where int8 was meant — is the
/// failure mode a prefix match invites, and it surfaces as gibberish rather
/// than as an error.
pub fn find_streaming_model_files(model_dir: &Path) -> Result<StreamingModelFiles, InferError> {
    let tokens = model_dir.join("tokens.txt");
    if !tokens.is_file() {
        return Err(InferError::Other(format!(
            "missing tokens.txt in {}",
            model_dir.display()
        )));
    }

    // Read once: three prefix scans over one listing, not three directory
    // walks, and a stable order so two runs cannot pick different files.
    let mut names: Vec<String> = std::fs::read_dir(model_dir)
        .map_err(|e| {
            InferError::Other(format!(
                "cannot read the model directory {}: {e}",
                model_dir.display()
            ))
        })?
        .flatten()
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();

    let encoder = pick_role(&names, "encoder", Quantization::PreferInt8, model_dir)?;
    let decoder = pick_role(&names, "decoder", Quantization::PreferFloat, model_dir)?;
    let joiner = pick_role(&names, "joiner", Quantization::PreferInt8, model_dir)?;

    Ok(StreamingModelFiles {
        encoder: model_dir.join(encoder),
        decoder: model_dir.join(decoder),
        joiner: model_dir.join(joiner),
        tokens,
    })
}

fn pick_role(
    names: &[String],
    role: &str,
    quantization: Quantization,
    model_dir: &Path,
) -> Result<String, InferError> {
    let candidates: Vec<&String> = names
        .iter()
        .filter(|name| name.starts_with(role) && name.ends_with(".onnx"))
        .collect();

    let wanted_int8 = quantization == Quantization::PreferInt8;
    let chosen = candidates
        .iter()
        .find(|name| name.contains(".int8.") == wanted_int8)
        // The preference is a preference: a bundle that ships only one
        // precision still loads rather than reporting a file it does have as
        // missing.
        .or(candidates.first())
        .copied();

    chosen.cloned().ok_or_else(|| {
        InferError::Other(format!(
            "no {role}*.onnx in {} (found: {})",
            model_dir.display(),
            if names.is_empty() {
                "nothing".to_string()
            } else {
                names.join(", ")
            }
        ))
    })
}

#[cfg(feature = "sherpa")]
pub struct SherpaOnnxStreamingInference;

#[cfg(feature = "sherpa")]
impl SherpaOnnxStreamingInference {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "sherpa")]
impl Default for SherpaOnnxStreamingInference {
    fn default() -> Self {
        Self::new()
    }
}

/// Builds the sherpa config from [`StreamingCfg`].
///
/// Every assignment is mandatory — see [`StreamingCfg`] for what
/// `OnlineRecognizerConfig::default()` leaves behind. `model_type` is
/// deliberately **not** set: the offline path names `"nemo_transducer"`
/// (`sherpa_stt.rs`) because a NeMo export needs telling, but a streaming
/// Zipformer transducer is what sherpa infers from a filled
/// `transducer` config, and naming a type it does not know makes `create`
/// return null with no diagnostic.
#[cfg(feature = "sherpa")]
fn streaming_config(
    files: &StreamingModelFiles,
    cfg: StreamingCfg,
) -> sherpa_onnx::OnlineRecognizerConfig {
    use sherpa_onnx::{OnlineRecognizerConfig, OnlineTransducerModelConfig};

    let mut config = OnlineRecognizerConfig::default();
    config.model_config.transducer = OnlineTransducerModelConfig {
        encoder: Some(files.encoder.to_string_lossy().into_owned()),
        decoder: Some(files.decoder.to_string_lossy().into_owned()),
        joiner: Some(files.joiner.to_string_lossy().into_owned()),
    };
    config.model_config.tokens = Some(files.tokens.to_string_lossy().into_owned());
    config.model_config.num_threads = cfg.num_threads;
    config.decoding_method = Some("greedy_search".into());
    config.enable_endpoint = cfg.enable_endpoint;
    config.rule1_min_trailing_silence = cfg.rule1_min_trailing_silence;
    config.rule2_min_trailing_silence = cfg.rule2_min_trailing_silence;
    config.rule3_min_utterance_length = cfg.rule3_min_utterance_length;
    config
}

#[cfg(feature = "sherpa")]
impl SherpaStreamingInference for SherpaOnnxStreamingInference {
    fn open(
        &self,
        model_dir: &Path,
        cfg: StreamingCfg,
    ) -> Result<Box<dyn SherpaStreamSession>, InferError> {
        use sherpa_onnx::OnlineRecognizer;

        let files = find_streaming_model_files(model_dir)?;
        let config = streaming_config(&files, cfg);
        let recognizer = OnlineRecognizer::create(&config).ok_or_else(|| {
            InferError::Other(format!(
                "failed to create a sherpa OnlineRecognizer from {}",
                model_dir.display()
            ))
        })?;
        let stream = recognizer.create_stream();
        Ok(Box::new(SherpaOnnxStreamSession {
            recognizer,
            stream,
            last_text: String::new(),
            endpointed: false,
            finished: false,
        }))
    }
}

#[cfg(feature = "sherpa")]
struct SherpaOnnxStreamSession {
    recognizer: sherpa_onnx::OnlineRecognizer,
    stream: sherpa_onnx::OnlineStream,
    last_text: String,
    endpointed: bool,
    finished: bool,
}

#[cfg(feature = "sherpa")]
impl SherpaOnnxStreamSession {
    /// Decode everything the stream is ready for and report the hypothesis if
    /// it moved. Shared by `accept` and `finish` so the readiness drain cannot
    /// drift between them.
    fn drain(&mut self) -> Option<String> {
        while self.recognizer.is_ready(&self.stream) {
            self.recognizer.decode(&self.stream);
        }
        let text = self
            .recognizer
            .get_result(&self.stream)
            .map(|result| result.text)
            .unwrap_or_default();
        if text == self.last_text {
            return None;
        }
        self.last_text = text.clone();
        Some(text)
    }
}

#[cfg(feature = "sherpa")]
impl SherpaStreamSession for SherpaOnnxStreamSession {
    fn accept(&mut self, pcm: AudioPcm) -> Result<Option<String>, InferError> {
        // Reset here rather than at the endpoint itself: the caller has to see
        // the text of the segment that just closed before the decoder forgets
        // it. So the previous call reports the endpoint, and this one starts
        // the new utterance.
        if self.endpointed {
            self.recognizer.reset(&self.stream);
            self.endpointed = false;
            self.last_text.clear();
        }
        self.stream
            .accept_waveform(pcm.sample_rate_hz as i32, &pcm.samples);
        let text = self.drain();
        self.endpointed = self.recognizer.is_endpoint(&self.stream);
        Ok(text)
    }

    fn endpointed(&self) -> bool {
        self.endpointed
    }

    fn finish(&mut self) -> Result<String, InferError> {
        if !self.finished {
            self.stream.input_finished();
            self.finished = true;
        }
        self.drain();
        Ok(self.last_text.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(files: &[&str]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in files {
            std::fs::write(dir.path().join(name), b"x").unwrap();
        }
        dir
    }

    /// The real layout of `sherpa-onnx-streaming-zipformer-en-20M-2023-02-17`
    /// and every other streaming Zipformer bundle: epoch/averaging suffixes,
    /// which is exactly what the offline finder's literal names miss.
    #[test]
    fn finds_epoch_suffixed_files_and_prefers_the_right_precision() {
        let dir = bundle(&[
            "tokens.txt",
            "encoder-epoch-99-avg-1.onnx",
            "encoder-epoch-99-avg-1.int8.onnx",
            "decoder-epoch-99-avg-1.onnx",
            "decoder-epoch-99-avg-1.int8.onnx",
            "joiner-epoch-99-avg-1.onnx",
            "joiner-epoch-99-avg-1.int8.onnx",
        ]);
        let found = find_streaming_model_files(dir.path()).unwrap();

        assert!(found.encoder.ends_with("encoder-epoch-99-avg-1.int8.onnx"));
        assert!(found.joiner.ends_with("joiner-epoch-99-avg-1.int8.onnx"));
        // The decoder is the one that stays fp32.
        assert!(found.decoder.ends_with("decoder-epoch-99-avg-1.onnx"));
        assert!(found.tokens.ends_with("tokens.txt"));
    }

    #[test]
    fn a_single_precision_bundle_still_loads() {
        let dir = bundle(&[
            "tokens.txt",
            "encoder-epoch-99-avg-1.onnx",
            "decoder-epoch-99-avg-1.int8.onnx",
            "joiner-epoch-99-avg-1.onnx",
        ]);
        let found = find_streaming_model_files(dir.path()).unwrap();
        assert!(found.encoder.ends_with("encoder-epoch-99-avg-1.onnx"));
        assert!(found.decoder.ends_with("decoder-epoch-99-avg-1.int8.onnx"));
    }

    /// A wrong file picked silently produces gibberish, so the error has to
    /// say what was actually there.
    #[test]
    fn a_missing_role_reports_the_directory_listing() {
        let dir = bundle(&[
            "tokens.txt",
            "encoder-epoch-99-avg-1.onnx",
            "decoder-epoch-99-avg-1.onnx",
        ]);
        let err = find_streaming_model_files(dir.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no joiner*.onnx"), "{err}");
        assert!(err.contains("encoder-epoch-99-avg-1.onnx"), "{err}");
    }

    #[test]
    fn a_bundle_without_tokens_is_not_a_bundle() {
        let dir = bundle(&["encoder-epoch-99-avg-1.onnx"]);
        let err = find_streaming_model_files(dir.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("tokens.txt"), "{err}");
    }

    /// Nothing in the bundle is addressed by a bare name, so a stray file that
    /// merely contains "encoder" must not be mistaken for one.
    #[test]
    fn non_onnx_files_are_ignored() {
        let dir = bundle(&[
            "tokens.txt",
            "encoder-epoch-99-avg-1.onnx",
            "encoder-epoch-99-avg-1.onnx.md5",
            "decoder-epoch-99-avg-1.onnx",
            "joiner-epoch-99-avg-1.onnx",
        ]);
        let found = find_streaming_model_files(dir.path()).unwrap();
        assert!(found.encoder.ends_with("encoder-epoch-99-avg-1.onnx"));
    }
}
