//! Non-macOS stub.
//!
//! Refuses by name rather than returning an empty transcript: a recognizer
//! that answers "" on Windows would look like silence, and the picker that
//! offered it would have no way to know it could never work.

use std::path::Path;

use super::{SpeechAuth, SpeechError, SpeechOpts, SpeechRecognition, SpeechTranscript};

pub struct StubSpeechRecognition;

impl StubSpeechRecognition {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubSpeechRecognition {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeechRecognition for StubSpeechRecognition {
    fn authorization(&self) -> SpeechAuth {
        SpeechAuth::Unavailable
    }

    fn request_authorization(&self) -> SpeechAuth {
        SpeechAuth::Unavailable
    }

    fn supports_on_device(&self, _locale: Option<&str>) -> bool {
        false
    }

    fn transcribe_file(
        &self,
        _path: &Path,
        _opts: &SpeechOpts,
    ) -> Result<SpeechTranscript, SpeechError> {
        Err(SpeechError::Unavailable)
    }
}
