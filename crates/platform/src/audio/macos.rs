//! macOS microphone capture via `cpal`, with meeting capture (mic ± loopback mix).
//!
//! # Manual verification
//! 1. Grant Microphone permission to the host app (System Settings > Privacy & Security).
//! 2. Call `start_mic()`, speak into the default input device, then `stop_mic()`.
//! 3. Returned [`PcmFrame`] should contain non-empty mono f32 samples at the device rate.
//! 4. `current_level()` should rise while audio is present.
//! 5. For meetings: `start_meeting()` → speak → `drain_meeting_buffer()` / `stop_meeting()`.
//! 6. Optional loopback: install BlackHole (or similar); `system_audio_capability()` →
//!    [`SystemAudioCapability::LoopbackDevice`]; route system audio to the virtual device.
//! 7. Optional SCK: build with `--features system-audio-sck`, grant Screen Recording;
//!    `system_audio_capability()` → [`SystemAudioCapability::ScreenCaptureKit`].
//! 8. Headless CI cannot access the mic — unit tests cover the state machine via
//!    [`MacAudioIo::new_for_test`] and the system-audio pump via a fake
//!    [`SystemAudioCapture`].

use super::loopback::find_loopback_input_device;
use super::macos_sck::{
    new_system_audio_capture, sck_feature_enabled, screen_recording_granted, SystemAudioCapture,
};
use super::util::{accumulate_frames, downmix_to_mono, mix_frames, rms_level};
use super::{AudioIo, AudioIoError, DictationState, MeetingState, PcmFrame, SystemAudioCapability};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

struct CaptureWorker {
    stop_tx: mpsc::Sender<()>,
    join: JoinHandle<()>,
}

struct MeetingCaptureWorker {
    mic: CaptureWorker,
    loopback: Option<CaptureWorker>,
    sck: Option<SckMeetingWorker>,
}

/// The pump thread that copies system-audio frames into the mic callback's mix
/// slot. The capture itself is owned by [`MacAudioIo::system_audio`], so the
/// backend outlives a single meeting.
struct SckMeetingWorker {
    pump: JoinHandle<()>,
}

pub struct MacAudioIo {
    dictation_state: Mutex<DictationState>,
    meeting_state: Mutex<MeetingState>,
    level: Arc<Mutex<f32>>,
    dictation_buffered: Arc<Mutex<Vec<PcmFrame>>>,
    meeting_drain_frames: Arc<Mutex<Vec<PcmFrame>>>,
    dictation_capture: Mutex<Option<CaptureWorker>>,
    meeting_capture: Mutex<Option<MeetingCaptureWorker>>,
    sample_rate_hz: Mutex<u32>,
    dictation_drops: Arc<AtomicU64>,
    meeting_mic_drops: Arc<AtomicU64>,
    /// System-audio backend behind the [`SystemAudioCapture`] seam: SCK when the
    /// `system-audio-sck` feature is on, otherwise the refusing null object.
    system_audio: Box<dyn SystemAudioCapture>,
}

impl Default for MacAudioIo {
    fn default() -> Self {
        Self::new()
    }
}

impl MacAudioIo {
    /// Production constructor; probes the default input device but does not start capture.
    pub fn new() -> Self {
        let sample_rate_hz = default_input_sample_rate().unwrap_or(48_000);
        Self::with_sample_rate(sample_rate_hz)
    }

    /// Test constructor — no `cpal` stream or device open; capability fixed to mic-only.
    pub fn new_for_test() -> Self {
        Self::with_sample_rate(48_000)
    }

    fn with_sample_rate(sample_rate_hz: u32) -> Self {
        Self {
            dictation_state: Mutex::new(DictationState::Idle),
            meeting_state: Mutex::new(MeetingState::Idle),
            level: Arc::new(Mutex::new(0.0)),
            dictation_buffered: Arc::new(Mutex::new(Vec::new())),
            meeting_drain_frames: Arc::new(Mutex::new(Vec::new())),
            dictation_capture: Mutex::new(None),
            meeting_capture: Mutex::new(None),
            sample_rate_hz: Mutex::new(sample_rate_hz),
            dictation_drops: Arc::new(AtomicU64::new(0)),
            meeting_mic_drops: Arc::new(AtomicU64::new(0)),
            system_audio: new_system_audio_capture(),
        }
    }
}

fn detect_system_audio_capability() -> SystemAudioCapability {
    if sck_feature_enabled() && screen_recording_granted() {
        return SystemAudioCapability::ScreenCaptureKit;
    }

    let host = cpal::default_host();
    if find_loopback_input_device(&host).is_some() {
        SystemAudioCapability::LoopbackDevice
    } else {
        SystemAudioCapability::MicOnly
    }
}

fn default_input_sample_rate() -> Option<u32> {
    let host = cpal::default_host();
    let device = host.default_input_device()?;
    let config = device.default_input_config().ok()?;
    Some(config.sample_rate().0)
}

fn f32_passthrough(sample: f32) -> f32 {
    sample
}

fn i16_to_f32(sample: i16) -> f32 {
    sample as f32 / i16::MAX as f32
}

fn u16_to_f32(sample: u16) -> f32 {
    (sample as f32 / u16::MAX as f32) * 2.0 - 1.0
}

fn push_dictation_frame(
    samples: Vec<f32>,
    sample_rate_hz: u32,
    level: &Arc<Mutex<f32>>,
    tx: &tokio::sync::mpsc::Sender<PcmFrame>,
    buffered: &Arc<Mutex<Vec<PcmFrame>>>,
    drops: &AtomicU64,
) {
    let frame = PcmFrame {
        samples,
        sample_rate_hz,
    };
    *level.lock().unwrap_or_else(|p| p.into_inner()) = rms_level(&frame.samples);
    if tx.try_send(frame.clone()).is_err() {
        let n = drops.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(100) {
            tracing::warn!(
                drops = n,
                "dictation: mic frame channel full; dropped {} audio frames so far",
                n
            );
        }
    }
    buffered
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(frame);
}

fn push_meeting_frame(
    mut frame: PcmFrame,
    level: &Arc<Mutex<f32>>,
    tx: &tokio::sync::mpsc::Sender<PcmFrame>,
    drain_frames: &Arc<Mutex<Vec<PcmFrame>>>,
    latest_loopback: Option<&Arc<Mutex<Option<PcmFrame>>>>,
    drops: &AtomicU64,
) {
    if let Some(lb) = latest_loopback {
        if let Some(ref sys) = *lb.lock().unwrap_or_else(|p| p.into_inner()) {
            frame = mix_frames(&frame, sys);
        }
    }
    *level.lock().unwrap_or_else(|p| p.into_inner()) = rms_level(&frame.samples);
    if tx.try_send(frame.clone()).is_err() {
        let n = drops.fetch_add(1, Ordering::Relaxed) + 1;
        if n.is_multiple_of(100) {
            tracing::warn!(
                drops = n,
                "meeting: mic frame channel full; dropped {} audio frames so far",
                n
            );
        }
    }
    drain_frames
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(frame);
}

fn push_loopback_frame(
    samples: Vec<f32>,
    sample_rate_hz: u32,
    latest: &Arc<Mutex<Option<PcmFrame>>>,
) {
    *latest.lock().unwrap_or_else(|p| p.into_inner()) = Some(PcmFrame {
        samples,
        sample_rate_hz,
    });
}

fn run_capture_on_device(
    device: Device,
    stop_rx: mpsc::Receiver<()>,
    frame_tx: tokio::sync::mpsc::Sender<PcmFrame>,
    dictation_buffered: Arc<Mutex<Vec<PcmFrame>>>,
    level: Arc<Mutex<f32>>,
    drops: Arc<AtomicU64>,
) -> Result<(), AudioIoError> {
    run_input_stream(device, stop_rx, move |mono, sample_rate_hz| {
        push_dictation_frame(
            mono,
            sample_rate_hz,
            &level,
            &frame_tx,
            &dictation_buffered,
            &drops,
        );
    })
}

fn run_meeting_mic_capture(
    device: Device,
    stop_rx: mpsc::Receiver<()>,
    frame_tx: tokio::sync::mpsc::Sender<PcmFrame>,
    drain_frames: Arc<Mutex<Vec<PcmFrame>>>,
    level: Arc<Mutex<f32>>,
    latest_loopback: Option<Arc<Mutex<Option<PcmFrame>>>>,
    drops: Arc<AtomicU64>,
) -> Result<(), AudioIoError> {
    run_input_stream(device, stop_rx, move |mono, sample_rate_hz| {
        let frame = PcmFrame {
            samples: mono,
            sample_rate_hz,
        };
        push_meeting_frame(
            frame,
            &level,
            &frame_tx,
            &drain_frames,
            latest_loopback.as_ref(),
            &drops,
        );
    })
}

/// Start system-audio capture and pump its frames into `latest`, the slot the
/// meeting mic callback mixes from. The capture stays owned by [`MacAudioIo`]
/// so `stop_meeting` can stop the same instance.
async fn start_system_audio(
    capture: &mut dyn SystemAudioCapture,
    latest: Arc<Mutex<Option<PcmFrame>>>,
) -> Result<SckMeetingWorker, AudioIoError> {
    let mut frame_rx = capture.start().await?;
    let pump = thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime for SCK pump");
        rt.block_on(async move {
            while let Some(frame) = frame_rx.recv().await {
                *latest.lock().unwrap_or_else(|p| p.into_inner()) = Some(frame);
            }
        });
    });
    Ok(SckMeetingWorker { pump })
}

fn run_loopback_capture(
    device: Device,
    stop_rx: mpsc::Receiver<()>,
    latest: Arc<Mutex<Option<PcmFrame>>>,
) -> Result<(), AudioIoError> {
    run_input_stream(device, stop_rx, move |mono, sample_rate_hz| {
        push_loopback_frame(mono, sample_rate_hz, &latest);
    })
}

/// Everything `build_mono_input_stream` needs that does not depend on the
/// device's sample format, so the format match stays one line per arm.
struct StreamSpec<'a> {
    device: &'a Device,
    config: &'a cpal::StreamConfig,
    channels: usize,
    sample_rate_hz: u32,
}

impl StreamSpec<'_> {
    /// Open an input stream of sample type `S`, downmixing each buffer to mono
    /// f32 with `to_f32` before handing it to `callback`.
    fn build_mono_input_stream<S, F>(
        &self,
        to_f32: fn(S) -> f32,
        callback: &Arc<F>,
    ) -> Result<cpal::Stream, AudioIoError>
    where
        S: cpal::SizedSample + 'static,
        F: Fn(Vec<f32>, u32) + Send + Sync + 'static,
    {
        let cb = Arc::clone(callback);
        let channels = self.channels;
        let sample_rate_hz = self.sample_rate_hz;
        self.device
            .build_input_stream(
                self.config,
                move |data: &[S], _| {
                    let mono = downmix_to_mono(data, channels, to_f32);
                    cb(mono, sample_rate_hz);
                },
                |err| tracing::error!("audio input stream error: {err}"),
                None,
            )
            .map_err(|e| AudioIoError::Other(e.to_string()))
    }
}

fn run_input_stream<F>(
    device: Device,
    stop_rx: mpsc::Receiver<()>,
    on_samples: F,
) -> Result<(), AudioIoError>
where
    F: Fn(Vec<f32>, u32) + Send + Sync + 'static,
{
    let callback = Arc::new(on_samples);

    let config = device
        .default_input_config()
        .map_err(|e| AudioIoError::Other(e.to_string()))?;

    let sample_rate_hz = config.sample_rate().0;
    let channels = config.channels() as usize;
    let stream_config: cpal::StreamConfig = config.clone().into();

    let build = StreamSpec {
        device: &device,
        config: &stream_config,
        channels,
        sample_rate_hz,
    };

    let stream = match config.sample_format() {
        SampleFormat::F32 => build.build_mono_input_stream(f32_passthrough, &callback)?,
        SampleFormat::I16 => build.build_mono_input_stream(i16_to_f32, &callback)?,
        SampleFormat::U16 => build.build_mono_input_stream(u16_to_f32, &callback)?,
        other => {
            return Err(AudioIoError::Other(format!(
                "unsupported sample format: {other:?}"
            )));
        }
    };

    stream
        .play()
        .map_err(|e| AudioIoError::Other(e.to_string()))?;

    let _ = stop_rx.recv();
    Ok(())
}

fn stop_worker(worker: CaptureWorker) -> Result<(), AudioIoError> {
    let _ = worker.stop_tx.send(());
    worker
        .join
        .join()
        .map_err(|_| AudioIoError::Other("capture thread panicked".into()))?;
    Ok(())
}

#[async_trait]
impl AudioIo for MacAudioIo {
    async fn start_mic(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
        if *self
            .dictation_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            != DictationState::Idle
        {
            return Err(AudioIoError::Other("mic already active".into()));
        }
        if *self.meeting_state.lock().unwrap_or_else(|p| p.into_inner()) != MeetingState::Idle {
            return Err(AudioIoError::Other("meeting capture active".into()));
        }

        self.dictation_buffered
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;

        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AudioIoError::Other("no input device".into()))?;

        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(64);
        let (stop_tx, stop_rx) = mpsc::channel();
        let buffered = Arc::clone(&self.dictation_buffered);
        let level = Arc::clone(&self.level);
        let drops = Arc::clone(&self.dictation_drops);
        drops.store(0, Ordering::Relaxed);

        let join = thread::spawn(move || {
            if let Err(err) =
                run_capture_on_device(device, stop_rx, frame_tx, buffered, level, drops)
            {
                tracing::error!("mic capture failed: {err}");
            }
        });

        if let Some(rate) = default_input_sample_rate() {
            *self
                .sample_rate_hz
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = rate;
        }

        *self
            .dictation_state
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = DictationState::Listening;
        *self
            .dictation_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(CaptureWorker { stop_tx, join });

        Ok(frame_rx)
    }

    async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
        if *self
            .dictation_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            != DictationState::Listening
        {
            return Err(AudioIoError::Other("mic not active".into()));
        }

        if let Some(worker) = self
            .dictation_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            stop_worker(worker)?;
        }

        let drops = self.dictation_drops.swap(0, Ordering::Relaxed);
        if drops > 0 {
            tracing::warn!(
                drops,
                "dictation: {} mic frames dropped (channel full) during this session",
                drops
            );
        }

        let frames = std::mem::take(
            &mut *self
                .dictation_buffered
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        );
        *self
            .dictation_state
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = DictationState::Idle;
        *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;

        Ok(accumulate_frames(&frames))
    }

    fn current_level(&self) -> f32 {
        *self.level.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn state(&self) -> DictationState {
        *self
            .dictation_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    fn system_audio_capability(&self) -> SystemAudioCapability {
        detect_system_audio_capability()
    }

    fn meeting_state(&self) -> MeetingState {
        *self.meeting_state.lock().unwrap_or_else(|p| p.into_inner())
    }

    async fn start_meeting(
        &mut self,
        prefer_system_audio: bool,
    ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
        if *self
            .dictation_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            != DictationState::Idle
        {
            return Err(AudioIoError::Other("dictation active".into()));
        }
        if *self.meeting_state.lock().unwrap_or_else(|p| p.into_inner()) != MeetingState::Idle {
            return Err(AudioIoError::Other("meeting already active".into()));
        }

        self.meeting_drain_frames
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;

        let host = cpal::default_host();
        let mic_device = host
            .default_input_device()
            .ok_or_else(|| AudioIoError::Other("no input device".into()))?;

        let capability = self.system_audio_capability();

        let loopback_device = if prefer_system_audio {
            match capability {
                SystemAudioCapability::LoopbackDevice => find_loopback_input_device(&host),
                _ => None,
            }
        } else {
            None
        };

        let use_sck =
            prefer_system_audio && matches!(capability, SystemAudioCapability::ScreenCaptureKit);

        let latest_loopback = if loopback_device.is_some() || use_sck {
            Some(Arc::new(Mutex::new(None::<PcmFrame>)))
        } else {
            None
        };

        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(64);
        let (mic_stop_tx, mic_stop_rx) = mpsc::channel();
        let drain_frames = Arc::clone(&self.meeting_drain_frames);
        let level = Arc::clone(&self.level);
        let loopback_for_mic = latest_loopback.clone();
        let drops = Arc::clone(&self.meeting_mic_drops);
        drops.store(0, Ordering::Relaxed);

        let mic_join = thread::spawn(move || {
            if let Err(err) = run_meeting_mic_capture(
                mic_device,
                mic_stop_rx,
                frame_tx,
                drain_frames,
                level,
                loopback_for_mic,
                drops,
            ) {
                tracing::error!("meeting mic capture failed: {err}");
            }
        });

        let loopback_worker =
            if let (Some(device), Some(latest)) = (loopback_device, latest_loopback.clone()) {
                let (loop_stop_tx, loop_stop_rx) = mpsc::channel();
                let join = thread::spawn(move || {
                    if let Err(err) = run_loopback_capture(device, loop_stop_rx, latest) {
                        tracing::error!("loopback capture failed: {err}");
                    }
                });
                Some(CaptureWorker {
                    stop_tx: loop_stop_tx,
                    join,
                })
            } else {
                None
            };

        let sck_worker = if use_sck {
            let latest = latest_loopback
                .clone()
                .expect("SCK meeting capture requires loopback state");
            Some(start_system_audio(self.system_audio.as_mut(), latest).await?)
        } else {
            None
        };

        if let Some(rate) = default_input_sample_rate() {
            *self
                .sample_rate_hz
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = rate;
        }

        *self.meeting_state.lock().unwrap_or_else(|p| p.into_inner()) = MeetingState::Recording;
        *self
            .meeting_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(MeetingCaptureWorker {
            mic: CaptureWorker {
                stop_tx: mic_stop_tx,
                join: mic_join,
            },
            loopback: loopback_worker,
            sck: sck_worker,
        });

        Ok(frame_rx)
    }

    async fn stop_meeting(&mut self) -> Result<PcmFrame, AudioIoError> {
        if *self.meeting_state.lock().unwrap_or_else(|p| p.into_inner()) != MeetingState::Recording
        {
            return Err(AudioIoError::Other("meeting not active".into()));
        }

        let worker = self
            .meeting_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        if let Some(worker) = worker {
            stop_worker(worker.mic)?;
            if let Some(loopback) = worker.loopback {
                stop_worker(loopback)?;
            }
            if let Some(sck) = worker.sck {
                let _ = self.system_audio.stop().await;
                let _ = sck.pump.join();
            }
        }

        self.meeting_drain_frames
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        *self.meeting_state.lock().unwrap_or_else(|p| p.into_inner()) = MeetingState::Idle;
        *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;

        let drops = self.meeting_mic_drops.swap(0, Ordering::Relaxed);
        if drops > 0 {
            tracing::warn!(
                drops,
                "meeting: {} mic frames dropped (channel full) during this session",
                drops
            );
        }

        Ok(PcmFrame {
            samples: vec![],
            sample_rate_hz: *self
                .sample_rate_hz
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        })
    }

    async fn drain_meeting_buffer(&mut self) -> Result<PcmFrame, AudioIoError> {
        let frames = std::mem::take(
            &mut *self
                .meeting_drain_frames
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        );
        Ok(accumulate_frames(&frames))
    }

    async fn try_drain_meeting_segment(
        &mut self,
        cfg: crate::audio::segment::SegmentCutConfig,
    ) -> Result<Option<crate::audio::SpeechSegment>, AudioIoError> {
        let mut guard = self
            .meeting_drain_frames
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let pending = accumulate_frames(&guard);
        let Some(cut) =
            crate::audio::segment::find_segment_cut(&pending.samples, pending.sample_rate_hz, cfg)
        else {
            // Not at a cut point yet — leave everything buffered.
            return Ok(None);
        };

        // Keep whatever follows the cut as the start of the next segment, so
        // audio spoken after the pause is not discarded.
        let remainder = pending.samples[cut.take..].to_vec();
        *guard = if remainder.is_empty() {
            Vec::new()
        } else {
            vec![PcmFrame {
                samples: remainder,
                sample_rate_hz: pending.sample_rate_hz,
            }]
        };

        Ok(Some(crate::audio::SpeechSegment {
            pcm: PcmFrame {
                samples: pending.samples[..cut.take].to_vec(),
                sample_rate_hz: pending.sample_rate_hz,
            },
            has_speech: cut.has_speech,
        }))
    }

    async fn play(&self, pcm: PcmFrame) -> Result<(), AudioIoError> {
        tokio::task::spawn_blocking(move || crate::audio::playback::play_pcm_blocking(&pcm))
            .await
            .map_err(|e| AudioIoError::Other(e.to_string()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::loopback::is_loopback_device_name;
    use crate::audio::macos_sck::{FakeSck, UnavailableSystemAudioCapture};

    #[test]
    fn dictation_state_starts_idle() {
        let io = MacAudioIo::new_for_test();
        assert_eq!(io.state(), DictationState::Idle);
        assert_eq!(io.current_level(), 0.0);
    }

    #[test]
    fn meeting_state_starts_idle() {
        let io = MacAudioIo::new_for_test();
        assert_eq!(io.meeting_state(), MeetingState::Idle);
        // capability re-detects on each call; just ensure it doesn't panic
        let _ = io.system_audio_capability();
    }

    #[test]
    fn loopback_name_heuristic_matches_blackhole() {
        assert!(is_loopback_device_name("BlackHole 2ch"));
    }

    #[tokio::test]
    async fn system_audio_pump_publishes_frames_for_the_mic_mix() {
        let mut capture = FakeSck;
        let latest = Arc::new(Mutex::new(None));
        let worker = start_system_audio(&mut capture, Arc::clone(&latest))
            .await
            .expect("fake backend starts");
        // The fake closes its channel after one frame, so the pump exits on its own.
        worker.pump.join().expect("pump thread");
        let frame = latest.lock().unwrap().clone().expect("frame published");
        assert_eq!(frame.samples.len(), 100);
        assert_eq!(frame.sample_rate_hz, 48_000);
    }

    #[tokio::test]
    async fn unavailable_system_audio_refuses_to_start() {
        let mut capture = UnavailableSystemAudioCapture;
        let Err(AudioIoError::Other(msg)) =
            start_system_audio(&mut capture, Arc::new(Mutex::new(None))).await
        else {
            panic!("null object must refuse to start");
        };
        assert!(msg.contains("system-audio-sck"));
    }
}
