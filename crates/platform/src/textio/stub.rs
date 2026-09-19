//! Non-macOS stub until Windows/Linux platform tasks land.

use super::{ReplaceMode, TextIo, TextIoError};
use async_trait::async_trait;

pub struct StubTextIo;

impl StubTextIo {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TextIo for StubTextIo {
    async fn capture_selection(&self) -> Result<String, TextIoError> {
        Err(TextIoError::Other(
            "text I/O is not yet implemented on this platform".into(),
        ))
    }

    /// `replace` and `insert_at_cursor` are the trait's defaults over this, so
    /// all three report the same thing.
    async fn replace_with_mode(&self, _text: &str, _mode: ReplaceMode) -> Result<(), TextIoError> {
        Err(TextIoError::Other(
            "text I/O is not yet implemented on this platform".into(),
        ))
    }
}
