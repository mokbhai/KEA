//! Non-macOS screen stubs — region capture and OCR error on use.
//!
//! Same shape as `textio/stub.rs`: construction succeeds so the composition
//! root builds everywhere, and the first real call says plainly that this
//! platform cannot do it. [`StubTextRecognizer::supported_languages`] is the
//! one exception — an empty list is the honest answer, and a settings page
//! that renders an empty picker is better than one that renders an error.

use std::path::Path;

use async_trait::async_trait;

use super::{CaptureOutcome, Observation, OcrOptions, ScreenCapture, ScreenError, TextRecognizer};

const UNSUPPORTED: &str = "screen capture and OCR are only implemented on macOS";

pub struct StubScreenCapture;

impl StubScreenCapture {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubScreenCapture {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ScreenCapture for StubScreenCapture {
    fn availability(&self) -> Result<(), ScreenError> {
        Err(ScreenError::Unavailable(UNSUPPORTED.into()))
    }

    async fn capture_region(&self) -> Result<CaptureOutcome, ScreenError> {
        Err(ScreenError::Unavailable(UNSUPPORTED.into()))
    }
}

pub struct StubTextRecognizer;

impl StubTextRecognizer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubTextRecognizer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TextRecognizer for StubTextRecognizer {
    fn supported_languages(&self) -> Result<Vec<String>, ScreenError> {
        Ok(Vec::new())
    }

    async fn recognize(
        &self,
        _image: &Path,
        _opts: &OcrOptions,
    ) -> Result<Vec<Observation>, ScreenError> {
        Err(ScreenError::Unavailable(UNSUPPORTED.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_stubs_error_rather_than_pretending() {
        let capture = StubScreenCapture::new();
        assert!(capture.availability().is_err());
        assert!(capture.capture_region().await.is_err());

        let recognizer = StubTextRecognizer::new();
        assert!(recognizer.supported_languages().unwrap().is_empty());
        assert!(recognizer
            .recognize(Path::new("/tmp/region.png"), &OcrOptions::default())
            .await
            .is_err());
    }
}
