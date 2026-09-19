//! No system synthesizer off macOS: the trait is honoured by refusing, so a
//! caller gets a message rather than a missing symbol at link time.

use async_trait::async_trait;

use super::{SystemTtsError, SystemTtsInference, SystemVoice};
use crate::audio::PcmFrame;

pub struct StubSystemTts;

impl StubSystemTts {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubSystemTts {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SystemTtsInference for StubSystemTts {
    /// Empty rather than an error: "this platform has no system voices" is
    /// what an empty picker already says, and a listing is not a failure.
    fn voices(&self) -> Vec<SystemVoice> {
        Vec::new()
    }

    async fn synthesize(
        &self,
        _text: &str,
        _voice_id: Option<&str>,
        _speed: f32,
    ) -> Result<PcmFrame, SystemTtsError> {
        Err(SystemTtsError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_stub_lists_nothing_and_refuses_to_speak() {
        let tts = StubSystemTts::new();
        assert!(tts.voices().is_empty());
        let err = tts.synthesize("hello", None, 1.0).await.unwrap_err();
        assert!(matches!(err, SystemTtsError::Unsupported));
    }
}
