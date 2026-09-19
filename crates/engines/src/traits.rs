use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("authentication failed: {0} — check the API key in provider settings")]
    Auth(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    /// A rate limit or a server-side fault: the same request may well work a
    /// moment later, which `Http` gives the caller no way to tell.
    #[error("HTTP {status}: {body} — temporary, try again in a moment")]
    Retryable { status: u16, body: String },
    #[error("bad configuration: {0}")]
    Config(String),
    #[error("model not installed: {0} — download it in Settings")]
    ModelNotInstalled(String),
    #[error("{0}")]
    Other(String),
}

impl EngineError {
    pub fn http(status: u16, body: String) -> Self {
        EngineError::Http {
            status,
            body: Self::preview(body),
        }
    }

    pub fn retryable(status: u16, body: String) -> Self {
        EngineError::Retryable {
            status,
            body: Self::preview(body),
        }
    }

    /// A 401/403 from the server: the key is wrong, not merely absent, so the
    /// user gets the same "check the API key" advice as a missing one. The
    /// status and body stay in the message so the cause is still visible.
    pub fn auth_rejected(status: u16, body: String) -> Self {
        EngineError::Auth(format!("HTTP {status}: {}", Self::preview(body)))
    }

    /// Trims a response body down to something that fits in an error message.
    fn preview(body: String) -> String {
        if body.len() > 200 {
            let truncated: String = body.chars().take(200).collect();
            format!("{}…", truncated)
        } else {
            body
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineCaps {
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LlmRequest {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
    /// Which provider's key and base URL to use for this one call.
    ///
    /// An engine is registered once, under its own id, so a single
    /// `openai-compatible` instance serves *every* user-added provider. Its
    /// own `provider_ref` field is therefore only a default — the binding the
    /// resolver picked is what actually names the provider. Without this the
    /// binding's `provider_ref` died at the engine boundary and every custom
    /// provider was billed to the built-in `local-llm` ref, whose keychain
    /// entry does not exist: "missing api key" for a key the user had just
    /// saved. `SttOpts`/`TtsOpts` already carry the same field; the LLM path
    /// was the one that did not.
    #[serde(default)]
    pub provider_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmResponse {
    pub text: String,
}

#[async_trait]
pub trait LlmEngine: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EngineCaps;
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioPcm {
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SttOpts {
    pub model: Option<String>,
    pub language: Option<String>,
    pub provider_ref: Option<String>,
    /// Canonical spellings to bias decoding toward (names, acronyms, jargon).
    ///
    /// Advisory: every backend takes these differently and some ignore them
    /// entirely, which is why the transcript also goes through
    /// `kea_core::dictation::apply_vocabulary` afterwards. Empty means no hint,
    /// never an error.
    pub vocabulary: Vec<String>,
}

/// One timed span of recognized speech.
///
/// Offsets are milliseconds from the start of *whatever audio the caller
/// handed the engine*, and it is the caller — the file-transcription chunk
/// driver, the meeting poll — that rebases them onto the source timeline. An
/// engine has no way to know where its buffer sat in a longer recording, so a
/// segment that claimed absolute time would be lying at every call site but
/// one.
///
/// Integers rather than floats: these are written to SQLite, compared for
/// overlap, and formatted into subtitle timestamps, and a float would make
/// every one of those three answer "00:00:01,000" or "00:00:00,999"
/// depending on the rounding of the day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SttSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

impl SttSegment {
    pub fn new(start_ms: u64, end_ms: u64, text: impl Into<String>) -> Self {
        Self {
            start_ms,
            end_ms,
            text: text.into(),
        }
    }

    /// Shifts this segment onto a longer timeline. Saturating, because a
    /// chunk base plus a bogus engine offset must not wrap into the past.
    pub fn shifted(&self, base_ms: u64) -> Self {
        Self {
            start_ms: self.start_ms.saturating_add(base_ms),
            end_ms: self.end_ms.saturating_add(base_ms),
            text: self.text.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    /// Timed spans, when the engine reported any.
    ///
    /// Empty is the honest answer for a backend that reports no timing, not a
    /// reason to synthesize one span covering the whole buffer: the chunk
    /// driver already knows the chunk's extent and can do that far better
    /// than an engine that only knows it decoded *something*. `serde(default)`
    /// so a payload written before this field still deserializes.
    #[serde(default)]
    pub segments: Vec<SttSegment>,
}

impl Transcript {
    /// A transcript with no timing, which is what most call sites want.
    pub fn text_only(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            segments: Vec::new(),
        }
    }
}

#[async_trait]
pub trait SttEngine: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EngineCaps;
    async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError>;
}

/// One live hypothesis from a streaming recognizer.
///
/// Display only. The offline engine re-decodes the complete buffer when the
/// user stops and *that* is what gets inserted, so nothing here is ever an
/// input to insertion — which is what allows this path to be lossy, to fall
/// behind, and to be wrong.
#[derive(Debug, Clone, PartialEq)]
pub struct Partial {
    pub text: String,
    /// Increments each time the engine closes a segment; lets a consumer keep
    /// finished segments and replace only the tail.
    pub segment: u32,
    /// This partial closed a segment (a sherpa endpoint, or the provider
    /// equivalent). Never a signal to stop recording — the hotkey decides
    /// that, because a user pausing mid-sentence to think must not lose the
    /// rest of the sentence.
    pub endpoint: bool,
}

/// An engine that can produce live partial text while audio is still arriving.
///
/// Separate from [`SttEngine`] rather than an optional method on it: most
/// engines cannot stream, and a `transcribe`-shaped engine that answers
/// "unsupported" at run time is how a feature ends up silently inert.
#[async_trait]
pub trait StreamingSttEngine: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EngineCaps;
    /// [`EngineError::ModelNotInstalled`] when the streaming model is absent —
    /// which is the normal case, since nothing downloads it automatically.
    async fn open(&self, opts: SttOpts) -> Result<Box<dyn SttStream>, EngineError>;
}

/// A handle on one open streaming session.
///
/// A handle rather than a `Receiver<Partial>` the engine pumps into. Nothing
/// in this crate spawns tasks — every engine is a passive object the caller
/// drives — so a task-owning engine would need a runtime handle at
/// construction and make the fakes that carry the engine tests require live
/// timing. Cancellation is the other half: dictation ends when a key is
/// released, mid-utterance, arbitrarily often, and with a handle that is
/// `drop(stream)` with the borrow checker proving nothing else holds it.
#[async_trait]
pub trait SttStream: Send {
    /// Feed one capture frame. `Ok(None)` means the hypothesis did not change,
    /// which is what a greedy decoder answers most of the time.
    ///
    /// Must not block the runtime: an implementation backed by a blocking
    /// decoder hands the frame off and polls, rather than awaiting a decode.
    async fn accept(&mut self, audio: AudioPcm) -> Result<Option<Partial>, EngineError>;

    /// Flush trailing context and return the last hypothesis. Consumes the
    /// stream, so a session cannot be finalized twice.
    async fn finalize(self: Box<Self>) -> Result<Transcript, EngineError>;
}

#[derive(Debug, Clone, Default)]
pub struct TtsOpts {
    pub model: Option<String>,
    /// The speaker to use, by *name*.
    ///
    /// A name for every engine, including the local multi-speaker bundles
    /// that address speakers by integer id — the id is resolved from the name
    /// at synthesis time so the stored setting stays readable and survives a
    /// bundle that renumbers its table.
    pub voice: Option<String>,
    pub format: Option<String>,
    pub provider_ref: Option<String>,
    /// Rate multiplier, 1.0 being the voice's natural pace. `None` means the
    /// caller has no opinion, which is not the same as 1.0 for an engine that
    /// has its own default.
    pub speed: Option<f32>,
}

#[async_trait]
pub trait TtsEngine: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EngineCaps;
    async fn synthesize(&self, text: &str, opts: TtsOpts) -> Result<AudioPcm, EngineError>;
}

#[cfg(test)]
mod tts_types_tests {
    use super::*;

    struct EchoTts;

    #[async_trait]
    impl TtsEngine for EchoTts {
        fn id(&self) -> &str {
            "echo-tts"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps {
                models: vec!["echo".into()],
            }
        }

        async fn synthesize(&self, text: &str, _opts: TtsOpts) -> Result<AudioPcm, EngineError> {
            let n = text.len().min(100);
            Ok(AudioPcm {
                samples: vec![0.1; n * 100],
                sample_rate_hz: 24_000,
            })
        }
    }

    #[tokio::test]
    async fn tts_engine_synthesize_returns_pcm() {
        let engine = EchoTts;
        let pcm = engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap();
        assert_eq!(pcm.sample_rate_hz, 24_000);
        assert!(!pcm.samples.is_empty());
    }
}

#[cfg(test)]
mod stt_types_tests {
    use super::*;

    #[test]
    fn audio_pcm_holds_mono_samples() {
        let pcm = AudioPcm {
            samples: vec![0.0, 0.5, -0.5],
            sample_rate_hz: 16_000,
        };
        assert_eq!(pcm.samples.len(), 3);
        assert_eq!(pcm.sample_rate_hz, 16_000);
    }

    #[test]
    fn transcript_roundtrips_json() {
        let t = Transcript {
            text: "hello world".into(),
            segments: vec![SttSegment::new(0, 1_200, "hello world")],
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Transcript = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }

    /// A payload serialized before `segments` existed still has to decode —
    /// the field is `serde(default)` for exactly this.
    #[test]
    fn a_transcript_without_segments_still_decodes() {
        let back: Transcript = serde_json::from_str(r#"{"text":"hi"}"#).unwrap();
        assert_eq!(back, Transcript::text_only("hi"));
        assert!(back.segments.is_empty());
    }

    /// The chunk driver rebases every engine's segments onto the source
    /// timeline; getting this wrong makes every cue after the first chunk
    /// land at the wrong time, which is the item's named silent failure.
    #[test]
    fn shifting_moves_both_ends_by_the_base() {
        let seg = SttSegment::new(500, 1_500, "second chunk");
        let moved = seg.shifted(30_000);
        assert_eq!(moved.start_ms, 30_500);
        assert_eq!(moved.end_ms, 31_500);
        assert_eq!(moved.text, seg.text);
        // Saturating, so a nonsense offset cannot wrap into the past.
        assert_eq!(
            SttSegment::new(u64::MAX, u64::MAX, "x")
                .shifted(10)
                .start_ms,
            u64::MAX
        );
    }
}
