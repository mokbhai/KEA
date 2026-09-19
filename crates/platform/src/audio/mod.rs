//! Microphone capture and PCM frame types for dictation and meetings.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod cues;
pub mod decode;
#[cfg(target_os = "macos")]
pub mod loopback;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod macos_sck;
pub mod playback;
pub mod segment;
#[cfg(not(target_os = "macos"))]
pub mod stub;
pub mod util;

pub use cues::{cue_pcm, Cue};
pub use decode::{decode_file, is_probably_decodable, DecodeError, DECODE_SAMPLE_RATE_HZ};
pub use util::{
    accumulate_frames, choose_input_device, chunk_pcm_by_duration, cut_points, downmix_to_mono,
    mix_frames, resample_linear, rms_level, DeviceChoice, FrameCounters, RingBuffer,
};

/// Mono PCM samples at a specific sample rate (alias: capture buffer unit).
#[derive(Debug, Clone, PartialEq)]
pub struct PcmFrame {
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
}

/// Alias for [`PcmFrame`] used in dictation pipelines.
pub type PcmBuffer = PcmFrame;

/// A meeting segment taken at a cut point, with whether anyone actually spoke
/// in it. A speechless segment is dropped rather than transcribed: models
/// asked to transcribe silence tend to invent text.
#[derive(Debug, Clone, PartialEq)]
pub struct SpeechSegment {
    pub pcm: PcmFrame,
    pub has_speech: bool,
}

/// An input device the user can record from.
///
/// `id` is the device *name*, because a name is the only handle `cpal` offers.
/// It is not unique (two identical USB mics) and not stable across reboots on
/// every host, which is why selection resolves leniently — see
/// [`choose_input_device`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

/// Reported when the saved input device was not there and capture opened the
/// default instead. Surfaced to the user rather than logged: recording from
/// the wrong microphone for a whole meeting because a dock was unplugged is
/// the failure this exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceFallback {
    pub requested: String,
    /// The default device's name, or `None` when the host could not name it.
    pub using: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DictationState {
    Idle,
    Listening,
    /// Recording, started by a double tap of the hold chord and running until
    /// the next tap rather than until a key is released.
    ///
    /// The capture device cannot tell the two apart, so no [`AudioIo`] ever
    /// returns this: it is set by the app, which owns the lock, and published
    /// on `dictation:state` so the HUD can say the mic is open on purpose.
    Locked,
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
    async fn start_mic(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError>;

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
        Err(AudioIoError::Other(
            "meeting capture not implemented".into(),
        ))
    }

    /// Stop meeting capture; return full mixed mono PCM buffer.
    async fn stop_meeting(&mut self) -> Result<PcmFrame, AudioIoError> {
        let _ = self;
        Err(AudioIoError::Other(
            "meeting capture not implemented".into(),
        ))
    }

    /// Drain frames accumulated since last drain (for live segmented transcription).
    async fn drain_meeting_buffer(&mut self) -> Result<PcmFrame, AudioIoError> {
        Ok(PcmFrame {
            samples: vec![],
            sample_rate_hz: 16_000,
        })
    }

    /// Takes the next segment *if* the buffer has reached a good cut point —
    /// a pause in speech, or the configured maximum length. Returns `None`
    /// while the speaker is still mid-flow, leaving the audio buffered.
    ///
    /// This is what keeps a boundary from landing mid-word; callers poll it
    /// frequently rather than draining on a timer.
    async fn try_drain_meeting_segment(
        &mut self,
        cfg: segment::SegmentCutConfig,
    ) -> Result<Option<SpeechSegment>, AudioIoError> {
        let _ = cfg;
        Ok(None)
    }

    /// The input devices this host can record from.
    fn list_input_devices(&self) -> Vec<InputDevice> {
        Vec::new()
    }

    /// Set which device capture should open, by [`InputDevice::id`], or `None`
    /// for the OS default. Applied at the next stream open, never mid-capture.
    fn set_input_device(&mut self, preferred: Option<String>) {
        let _ = (self, preferred);
    }

    /// Take the fallback recorded by the last device resolution, if the saved
    /// device was missing.
    ///
    /// Taken rather than read so the report fires once per resolution — the
    /// alternative is a notification per audio callback.
    fn take_device_fallback(&mut self) -> Option<DeviceFallback> {
        None
    }

    /// Whether the input preview holds the capture device.
    fn preview_active(&self) -> bool {
        false
    }

    /// Open the input device purely to publish levels, discarding the audio.
    ///
    /// Takes the same capture gate as [`start_mic`](Self::start_mic) and
    /// [`start_meeting`](Self::start_meeting): the audio layer admits exactly
    /// one recorder, and a preview left running when a hotkey fires would
    /// otherwise open a second stream on the same device.
    async fn start_input_preview(&mut self) -> Result<(), AudioIoError> {
        let _ = self;
        Err(AudioIoError::Other("input preview not implemented".into()))
    }

    /// Stop the preview. A no-op when none is running, so timers, window blur
    /// and a dictation start can all call it without checking first.
    async fn stop_input_preview(&mut self) -> Result<(), AudioIoError> {
        let _ = self;
        Ok(())
    }

    /// Open the capture stream early and hold the most recent audio in a
    /// preroll ring, without starting a recording.
    ///
    /// Called when the first modifier of the hold chord has been down long
    /// enough to mean it; [`start_mic`](Self::start_mic) then adopts the armed
    /// stream and prepends the ring, which is the speech spoken before the
    /// hold threshold passed. Silently does nothing when anything else already
    /// holds the device — the preroll is an optimisation, never a failure.
    fn arm_capture(&mut self) {
        let _ = self;
    }

    /// Close an armed stream and drop its preroll. The chord was broken before
    /// it became a recording, so the audio is never transcribed.
    fn disarm_capture(&mut self) {
        let _ = self;
    }

    /// Whether a stream is armed and filling the preroll ring.
    fn is_armed(&self) -> bool {
        false
    }

    /// Play mono PCM to the default output device. Default impl is a no-op so fakes and stubs compile.
    async fn play(&self, pcm: PcmFrame) -> Result<(), AudioIoError> {
        let _ = pcm;
        Ok(())
    }
}

/// Construct the active platform [`AudioIo`] implementation for this OS.
pub fn new_audio_io() -> Box<dyn AudioIo> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacAudioIo::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(stub::StubAudioIo::new())
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
            Ok(self.pending_drains.pop().unwrap_or(PcmFrame {
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
        assert_eq!(
            io.system_audio_capability(),
            SystemAudioCapability::Unavailable
        );
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
            io.last_played
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .samples
                .len(),
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
