//! The operating system's own speech synthesizer.
//!
//! Platform rather than engines because this is an OS service, like
//! [`crate::macos_services`]: nothing is downloaded, the voices are whatever
//! the user has installed in System Settings, and the whole implementation is
//! a framework binding. The engine layer consumes it through
//! [`SystemTtsInference`], the same shape the ONNX voices are consumed by, so
//! the audio it produces goes down the existing playback path — cue sounds,
//! state machine and all.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::audio::PcmFrame;

#[cfg(target_os = "macos")]
pub mod macos_speech;
#[cfg(not(target_os = "macos"))]
pub mod stub;

/// One voice the OS offers, as the picker shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemVoice {
    /// The stable identifier to ask for this voice again
    /// ("com.apple.voice.compact.en-US.Samantha"). Names are neither unique
    /// nor stable across OS versions, so selection is stored by identifier.
    pub id: String,
    pub name: String,
    /// BCP-47 tag, e.g. "en-US".
    pub language: String,
    /// "default", "enhanced" or "premium" — the download tiers System
    /// Settings offers. Worth showing: the premium voices are the reason to
    /// use the system synthesizer at all.
    pub quality: String,
}

#[derive(Debug, Error)]
pub enum SystemTtsError {
    #[error("this platform has no system speech synthesizer")]
    Unsupported,
    #[error("no system voice with identifier '{0}' — it may need downloading in System Settings")]
    UnknownVoice(String),
    #[error("the system synthesizer produced no audio")]
    NoAudio,
    #[error("{0}")]
    Other(String),
}

/// Renders text with the OS synthesizer.
///
/// `synthesize` returns PCM rather than speaking: the direct speak path would
/// bypass the app's playback layer entirely, taking the cue sounds and the
/// read-aloud state machine with it.
#[async_trait]
pub trait SystemTtsInference: Send + Sync {
    /// Every installed voice, or empty where there is no synthesizer.
    fn voices(&self) -> Vec<SystemVoice>;

    /// `voice_id` is a [`SystemVoice::id`], or `None` for the system default.
    /// `speed` is a multiplier on the natural rate, 1.0 being unchanged.
    async fn synthesize(
        &self,
        text: &str,
        voice_id: Option<&str>,
        speed: f32,
    ) -> Result<PcmFrame, SystemTtsError>;
}

/// Construct the active platform [`SystemTtsInference`] for this OS.
pub fn new_system_tts() -> Box<dyn SystemTtsInference> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos_speech::MacSystemTts::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(stub::StubSystemTts::new())
    }
}
