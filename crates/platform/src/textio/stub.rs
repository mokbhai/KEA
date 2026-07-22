//! Non-macOS stub until Windows/Linux platform tasks land.

use super::{TextIo, TextIoError};
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

    async fn replace(&self, _text: &str) -> Result<(), TextIoError> {
        Err(TextIoError::Other(
            "text I/O is not yet implemented on this platform".into(),
        ))
    }

    async fn insert_at_cursor(&self, _text: &str) -> Result<(), TextIoError> {
        Err(TextIoError::Other(
            "text I/O is not yet implemented on this platform".into(),
        ))
    }
}
