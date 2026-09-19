//! Shared inference value types.
//!
//! These live apart from the engine modules because every backend in the
//! crate hands audio around in the same shape — `whisper`, `sherpa_stt` and
//! `sherpa_tts` all speak [`AudioPcm`] — and a value type owned by one
//! backend's module is how a parameter ends up on a trait that cannot honour
//! it.

/// Mono PCM samples in `[-1.0, 1.0]` at `sample_rate_hz`.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioPcm {
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
}

/// Decoding options for whisper.
///
/// Whisper-only on purpose: it is the one local backend whose model takes a
/// language. See [`crate::sherpa_stt::SherpaSttInference`] for why the ONNX
/// transducer does not take these.
#[derive(Debug, Clone, Default)]
pub struct WhisperOpts {
    pub language: Option<String>,
    /// Terms to seed whisper's initial prompt with. See
    /// [`crate::whisper`] for the token budget this is trimmed to.
    pub vocabulary: Vec<String>,
}

/// Per-request synthesis options for the local (sherpa-onnx) voices.
///
/// Separate from the catalog entry because both fields are *user* choices that
/// change per run, not properties of the bundle: `sid` is the speaker the user
/// picked out of a multi-speaker bundle, `speed` the rate they set. Kept in
/// this crate's shared types for the same reason as [`WhisperOpts`] — the
/// engine layer has to name them without depending on sherpa.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TtsSynthOpts {
    /// Rate multiplier, 1.0 being the voice's natural pace.
    pub speed: f32,
    /// Which speaker inside a multi-speaker bundle. 0 on every single-speaker
    /// model, which is what sherpa expects there too.
    pub sid: i32,
}

/// Slowest and fastest the rate may be set to.
///
/// Outside this range sherpa still synthesizes, but the result is unusable
/// rather than merely fast — and a 0 or negative multiplier makes it generate
/// nothing at all, which would surface as "TTS returned no audio".
pub const MIN_TTS_SPEED: f32 = 0.5;
pub const MAX_TTS_SPEED: f32 = 2.0;

/// Brings any stored or IPC-supplied rate into the usable range. NaN reads as
/// "no opinion" and takes the default rather than propagating into sherpa.
pub fn clamp_tts_speed(speed: f32) -> f32 {
    if speed.is_nan() {
        return 1.0;
    }
    speed.clamp(MIN_TTS_SPEED, MAX_TTS_SPEED)
}

impl Default for TtsSynthOpts {
    fn default() -> Self {
        Self { speed: 1.0, sid: 0 }
    }
}

impl TtsSynthOpts {
    pub fn new(speed: f32, sid: i32) -> Self {
        Self {
            speed: clamp_tts_speed(speed),
            sid: sid.max(0),
        }
    }
}

#[cfg(test)]
mod tts_opts_tests {
    use super::*;

    #[test]
    fn speed_is_clamped_into_the_usable_range() {
        assert_eq!(clamp_tts_speed(1.0), 1.0);
        assert_eq!(clamp_tts_speed(0.1), MIN_TTS_SPEED);
        assert_eq!(clamp_tts_speed(9.0), MAX_TTS_SPEED);
        // A zero multiplier makes sherpa emit nothing at all, which would
        // surface as "returned no audio" rather than as a bad setting.
        assert_eq!(clamp_tts_speed(0.0), MIN_TTS_SPEED);
        assert_eq!(clamp_tts_speed(-1.0), MIN_TTS_SPEED);
        assert_eq!(clamp_tts_speed(f32::NAN), 1.0);
    }

    #[test]
    fn negative_speaker_ids_are_refused_rather_than_passed_on() {
        assert_eq!(TtsSynthOpts::new(1.0, -3).sid, 0);
        assert_eq!(TtsSynthOpts::new(1.5, 4).sid, 4);
        assert_eq!(TtsSynthOpts::default(), TtsSynthOpts::new(1.0, 0));
    }
}

/// Sample rate the streaming recognizer is fed at.
///
/// Not a preference: `OnlineRecognizerConfig`'s feature extractor is built for
/// 16 kHz and sherpa resamples anything else itself, badly, per chunk.
pub const STREAMING_SAMPLE_RATE_HZ: u32 = 16_000;

/// How many decoder threads the *streaming* pass gets.
///
/// Two, not `available_parallelism()` like the offline transducer
/// (`sherpa_stt.rs`). The streaming pass is cosmetic and the offline pass
/// produces the text that is actually inserted; giving streaming every core
/// would buy invisible latency with visible latency.
pub const STREAMING_NUM_THREADS: i32 = 2;

/// Trailing silence, in seconds, that ends an utterance before any speech has
/// been decoded in it. Upstream's rule 1.
pub const STREAMING_RULE1_MIN_TRAILING_SILENCE: f32 = 2.4;

/// Trailing silence, in seconds, that ends an utterance that *did* contain
/// speech. Upstream's rule 2, and the one that fires in normal dictation.
pub const STREAMING_RULE2_MIN_TRAILING_SILENCE: f32 = 1.2;

/// Utterance length, in seconds, past which a segment is closed regardless of
/// silence. Upstream's rule 3.
///
/// Five minutes rather than upstream's 20 seconds: this is only a *display*
/// segmenter — see [`StreamingCfg`] — and the app's own lock mode already caps
/// a recording at five minutes, so a shorter rule would chop a long dictation
/// into segments for no benefit the user can see.
pub const STREAMING_RULE3_MIN_UTTERANCE_LENGTH: f32 = 300.0;

/// Configuration for one streaming recognition session.
///
/// Every field has to be set explicitly, which is the whole reason this type
/// exists. `OnlineRecognizerConfig::default()` in sherpa-onnx 1.13.3 leaves
/// `enable_endpoint: false` and all three rule thresholds at `0.0`
/// (`src/online_asr.rs`), which are *not* the upstream C++ defaults the sherpa
/// examples run with — and a `0.0` threshold is not "disabled". Upstream's
/// rule 1 has `must_contain_nonsilence = false` and `min_utterance_length = 0`,
/// so with `min_trailing_silence = 0.0` its match condition
/// (`trailing_silence >= min` and `utterance_length >= min`) is true on every
/// query: endpointing on with the crate defaults fires an endpoint per frame,
/// resetting the decoder continuously and showing the user one word at a time.
///
/// Endpointing here is a *text segmenter* and nothing else. It never stops the
/// recording — the hotkey does — because a user pausing mid-sentence to think
/// must not lose the rest of the sentence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StreamingCfg {
    pub num_threads: i32,
    pub enable_endpoint: bool,
    pub rule1_min_trailing_silence: f32,
    pub rule2_min_trailing_silence: f32,
    pub rule3_min_utterance_length: f32,
}

impl Default for StreamingCfg {
    fn default() -> Self {
        Self {
            num_threads: STREAMING_NUM_THREADS,
            enable_endpoint: true,
            rule1_min_trailing_silence: STREAMING_RULE1_MIN_TRAILING_SILENCE,
            rule2_min_trailing_silence: STREAMING_RULE2_MIN_TRAILING_SILENCE,
            rule3_min_utterance_length: STREAMING_RULE3_MIN_UTTERANCE_LENGTH,
        }
    }
}

#[cfg(test)]
mod streaming_cfg_tests {
    use super::*;

    /// The trap this type exists for, and it is invisible at the call site: a
    /// config that enables endpointing while leaving a threshold at zero
    /// endpoints on every frame.
    #[test]
    fn endpointing_is_on_and_no_threshold_is_zero() {
        let cfg = StreamingCfg::default();
        assert!(cfg.enable_endpoint);
        assert!(cfg.rule1_min_trailing_silence > 0.0);
        assert!(cfg.rule2_min_trailing_silence > 0.0);
        assert!(cfg.rule3_min_utterance_length > 0.0);
        // Rule 2 fires after speech, rule 1 before it; a rule 2 that waited
        // longer than rule 1 could never be the one to fire.
        assert!(cfg.rule2_min_trailing_silence < cfg.rule1_min_trailing_silence);
    }

    /// Streaming must not take the cores the offline pass needs: that pass
    /// produces the text that is actually inserted.
    #[test]
    fn streaming_leaves_cores_for_the_offline_pass() {
        assert_eq!(StreamingCfg::default().num_threads, 2);
    }
}
