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
}
