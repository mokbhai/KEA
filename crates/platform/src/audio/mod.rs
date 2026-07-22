//! Microphone capture and PCM frame types for dictation and meetings.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "macos")]
pub mod loopback;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod macos_sck;
#[cfg(not(target_os = "macos"))]
pub mod stub;
pub mod playback;
pub mod util;

pub use util::{accumulate_frames, chunk_pcm_by_duration, mix_frames, resample_linear, rms_level};

/// Mono PCM samples at a specific sample rate (alias: capture buffer unit).
#[derive(Debug, Clone, PartialEq)]
pub struct PcmFrame {
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
}

/// Alias for [`PcmFrame`] used in dictation pipelines.
pub type PcmBuffer = PcmFrame;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DictationState {
    Idle,
    Listening,
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeetingState {
    Idle,
    Recording,
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemAudioCapability {
    Unavailable,
    ScreenCaptureKit,
    LoopbackDevice,
    MicOnly,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum AudioIoError {
    #[error("{0}")]
    Other(String),
}

/// Push-to-talk microphone capture and meeting audio (mic ± system loopback).
#[async_trait]
pub trait AudioIo: Send + Sync {
    /// Begin mic capture; frames arrive via the returned receiver.
    async fn start_mic(
        &mut self,
    ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError>;

    /// Stop capture and return the full buffered mono PCM at the device's native rate.
    async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError>;

    /// RMS level of the most recent frame in \[0.0, 1.0\]; 0.0 when idle.
    fn current_level(&self) -> f32;

    fn state(&self) -> DictationState;

    /// Whether system/loopback audio can be captured alongside the mic.
    fn system_audio_capability(&self) -> SystemAudioCapability {
        SystemAudioCapability::Unavailable
    }

    fn meeting_state(&self) -> MeetingState {
        MeetingState::Idle
    }

    /// Begin meeting capture (mic + system when available). `prefer_system_audio` controls
    /// whether system/loopback capture is attempted when the platform supports it.
    async fn start_meeting(
        &mut self,
        prefer_system_audio: bool,
    ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
        let _ = (self, prefer_system_audio);
        Err(AudioIoError::Other("meeting capture not implemented".into()))
    }

    /// Stop meeting capture; return full mixed mono PCM buffer.
    async fn stop_meeting(&mut self) -> Result<PcmFrame, AudioIoError> {
        let _ = self;
        Err(AudioIoError::Other("meeting capture not implemented".into()))
    }

    /// Drain frames accumulated since last drain (for live segmented transcription).
    async fn drain_meeting_buffer(&mut self) -> Result<PcmFrame, AudioIoError> {
        Ok(PcmFrame {
            samples: vec![],
            sample_rate_hz: 16_000,
        })
    }

    /// Play mono PCM to the default output device. Default impl is a no-op so fakes and stubs compile.
    async fn play(&self, pcm: PcmFrame) -> Result<(), AudioIoError> {
        let _ = pcm;
        Ok(())
    }
}

#[cfg(test)]
mod audio_trait_tests {
    use super::*;
    use async_trait::async_trait;

    struct FakeAudioIo {
        state: DictationState,
        buffered: PcmFrame,
    }

    #[async_trait]
    impl AudioIo for FakeAudioIo {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.state = DictationState::Listening;
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            self.state = DictationState::Idle;
            Ok(self.buffered.clone())
        }

        fn current_level(&self) -> f32 {
            0.42
        }

        fn state(&self) -> DictationState {
            self.state
        }
    }

    #[tokio::test]
    async fn fake_audio_io_returns_buffered_pcm() {
        let mut io = FakeAudioIo {
            state: DictationState::Idle,
            buffered: PcmFrame {
                samples: vec![0.1, 0.2],
                sample_rate_hz: 48_000,
            },
        };
        let _rx = io.start_mic().await.unwrap();
        assert_eq!(io.state(), DictationState::Listening);
        let pcm = io.stop_mic().await.unwrap();
        assert_eq!(pcm.samples.len(), 2);
    }

    struct FakeMeetingAudioIo {
        dictation_state: DictationState,
        meeting_state: MeetingState,
        capability: SystemAudioCapability,
        buffered: PcmFrame,
        pending_drains: Vec<PcmFrame>,
    }

    #[async_trait]
    impl AudioIo for FakeMeetingAudioIo {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.dictation_state = DictationState::Listening;
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            self.dictation_state = DictationState::Idle;
            Ok(self.buffered.clone())
        }

        fn current_level(&self) -> f32 {
            0.0
        }

        fn state(&self) -> DictationState {
            self.dictation_state
        }

        fn system_audio_capability(&self) -> SystemAudioCapability {
            self.capability
        }

        fn meeting_state(&self) -> MeetingState {
            self.meeting_state
        }

        async fn start_meeting(
            &mut self,
            _prefer_system_audio: bool,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.meeting_state = MeetingState::Recording;
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn stop_meeting(&mut self) -> Result<PcmFrame, AudioIoError> {
            self.meeting_state = MeetingState::Idle;
            Ok(self.buffered.clone())
        }

        async fn drain_meeting_buffer(&mut self) -> Result<PcmFrame, AudioIoError> {
            Ok(self
                .pending_drains
                .pop()
                .unwrap_or(PcmFrame {
                    samples: vec![],
                    sample_rate_hz: 16_000,
                }))
        }
    }

    #[tokio::test]
    async fn fake_meeting_audio_drains_segments() {
        let mut io = FakeMeetingAudioIo {
            dictation_state: DictationState::Idle,
            meeting_state: MeetingState::Idle,
            capability: SystemAudioCapability::MicOnly,
            buffered: PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            },
            pending_drains: vec![PcmFrame {
                samples: vec![0.0; 1600],
                sample_rate_hz: 16_000,
            }],
        };
        let _rx = io.start_meeting(false).await.unwrap();
        assert_eq!(io.meeting_state(), MeetingState::Recording);
        let chunk = io.drain_meeting_buffer().await.unwrap();
        assert_eq!(chunk.samples.len(), 1600);
    }

    #[tokio::test]
    async fn default_meeting_methods_return_unsupported() {
        let mut io = FakeAudioIo {
            state: DictationState::Idle,
            buffered: PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            },
        };
        assert_eq!(io.system_audio_capability(), SystemAudioCapability::Unavailable);
        assert_eq!(io.meeting_state(), MeetingState::Idle);
        assert!(io.start_meeting(true).await.is_err());
        assert!(io.stop_meeting().await.is_err());
        let drained = io.drain_meeting_buffer().await.unwrap();
        assert!(drained.samples.is_empty());
    }

    struct FakePlayAudioIo {
        state: DictationState,
        buffered: PcmFrame,
        last_played: std::sync::Mutex<Option<PcmFrame>>,
    }

    #[async_trait]
    impl AudioIo for FakePlayAudioIo {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.state = DictationState::Listening;
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            self.state = DictationState::Idle;
            Ok(self.buffered.clone())
        }

        fn current_level(&self) -> f32 {
            0.0
        }

        fn state(&self) -> DictationState {
            self.state
        }

        async fn play(&self, pcm: PcmFrame) -> Result<(), AudioIoError> {
            *self.last_played.lock().unwrap() = Some(pcm);
            Ok(())
        }
    }

    #[tokio::test]
    async fn fake_audio_io_records_played_pcm() {
        let io = FakePlayAudioIo {
            state: DictationState::Idle,
            buffered: PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            },
            last_played: std::sync::Mutex::new(None),
        };
        let frame = PcmFrame {
            samples: vec![0.5; 100],
            sample_rate_hz: 48_000,
        };
        io.play(frame.clone()).await.unwrap();
        assert_eq!(
            io.last_played.lock().unwrap().as_ref().unwrap().samples.len(),
            100
        );
    }

    #[tokio::test]
    async fn default_play_is_noop() {
        let io = FakeAudioIo {
            state: DictationState::Idle,
            buffered: PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            },
        };
        io.play(PcmFrame {
            samples: vec![1.0],
            sample_rate_hz: 16_000,
        })
        .await
        .unwrap();
    }
}
