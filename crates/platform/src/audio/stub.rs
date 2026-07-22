//! Non-macOS stub until Windows/Linux platform audio tasks land.

use super::{AudioIo, AudioIoError, DictationState, PcmFrame};
use async_trait::async_trait;
use std::sync::Mutex;

pub struct StubAudioIo {
    state: Mutex<DictationState>,
}

impl StubAudioIo {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(DictationState::Idle),
        }
    }
}

#[async_trait]
impl AudioIo for StubAudioIo {
    async fn start_mic(
        &mut self,
    ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
        Err(AudioIoError::Other(
            "audio I/O is not yet implemented on this platform".into(),
        ))
    }

    async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
        Err(AudioIoError::Other(
            "audio I/O is not yet implemented on this platform".into(),
        ))
    }

    fn current_level(&self) -> f32 {
        0.0
    }

    fn state(&self) -> DictationState {
        *self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}
