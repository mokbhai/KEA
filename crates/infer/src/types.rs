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
