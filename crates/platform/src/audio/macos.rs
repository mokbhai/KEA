//! macOS microphone capture via `cpal`, with meeting capture (mic ± loopback mix).
//!
//! # Manual verification
//! 1. Grant Microphone permission to the host app (System Settings > Privacy & Security).
//! 2. Call `start_mic()`, speak into the default input device, then `stop_mic()`.
//! 3. Returned [`PcmFrame`] should contain non-empty mono f32 samples at the device rate.
//! 4. `current_level()` should rise while audio is present.
//! 5. For meetings: `start_meeting()` → speak → `drain_meeting_sources()` / `stop_meeting()`.
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
use super::util::{
    accumulate_frames, align_meeting_sources, choose_input_device, downmix_to_mono, rms_level,
    DeviceChoice, FrameCounters, RingBuffer,
};
use super::{
    AudioIo, AudioIoError, DeviceFallback, DictationState, InputDevice, MeetingState, PcmFrame,
    SystemAudioCapability,
};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, SampleFormat};
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

/// The pump thread that appends system-audio frames to the system drain
/// buffer. The capture itself is owned by [`MacAudioIo::system_audio`], so the
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
    /// System/loopback audio, buffered *beside* the mic rather than mixed into
    /// it. The two are mixed at drain time instead, which is the only place
    /// that can align them: a callback holds one mic frame and whatever the
    /// loopback thread happened to have produced by then, which is zero frames
    /// or two as often as it is one.
    meeting_system_frames: Arc<Mutex<Vec<PcmFrame>>>,
    /// RMS of the most recent system frame, so the meeting meter still shows
    /// the far side now that the mic callback no longer sees it.
    system_level: Arc<Mutex<f32>>,
    dictation_capture: Mutex<Option<CaptureWorker>>,
    meeting_capture: Mutex<Option<MeetingCaptureWorker>>,
    sample_rate_hz: Mutex<u32>,
    dictation_frames: FrameCounters,
    meeting_mic_frames: FrameCounters,
    /// System-audio backend behind the [`SystemAudioCapture`] seam: SCK when the
    /// `system-audio-sck` feature is on, otherwise the refusing null object.
    system_audio: Box<dyn SystemAudioCapture>,
    /// The device the user picked, by name, or `None` for the OS default.
    preferred_input_device: Mutex<Option<String>>,
    /// Set by the last resolution that could not find the preferred device,
    /// and taken by the app to tell the user which mic it is actually using.
    device_fallback: Mutex<Option<DeviceFallback>>,
    /// The level-only stream behind "Test microphone". Holds the capture gate
    /// exactly as a recording does — see [`MacAudioIo::capture_gate`].
    preview_capture: Mutex<Option<CaptureWorker>>,
    /// A stream opened ahead of the hold threshold, filling the preroll ring.
    armed_capture: Mutex<Option<ArmedCapture>>,
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
            meeting_system_frames: Arc::new(Mutex::new(Vec::new())),
            system_level: Arc::new(Mutex::new(0.0)),
            dictation_capture: Mutex::new(None),
            meeting_capture: Mutex::new(None),
            sample_rate_hz: Mutex::new(sample_rate_hz),
            dictation_frames: FrameCounters::new(),
            meeting_mic_frames: FrameCounters::new(),
            system_audio: new_system_audio_capture(),
            preferred_input_device: Mutex::new(None),
            device_fallback: Mutex::new(None),
            preview_capture: Mutex::new(None),
            armed_capture: Mutex::new(None),
        }
    }

    /// Empty both drain buffers, newest lock first.
    ///
    /// Taken rather than read under a held lock: everything the caller does
    /// with the frames — aligning, mixing, searching for a cut — runs over the
    /// whole buffered meeting, and a `cpal` callback blocked on one of these
    /// locks for that long is a dropped frame on a live device.
    fn take_pending_frames(&self) -> (Vec<PcmFrame>, Vec<PcmFrame>) {
        let mic = std::mem::take(
            &mut *self
                .meeting_drain_frames
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        );
        let system = std::mem::take(
            &mut *self
                .meeting_system_frames
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        );
        (mic, system)
    }

    /// Put frames back at the *front* of each drain buffer.
    ///
    /// At the front because callbacks kept running while the caller was
    /// looking at what it took: anything that landed in the meantime is newer
    /// than what is being returned, and appending would play the meeting back
    /// out of order.
    fn restore_pending_frames(&self, mic: Vec<PcmFrame>, system: Vec<PcmFrame>) {
        if !mic.is_empty() {
            self.meeting_drain_frames
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .splice(0..0, mic);
        }
        if !system.is_empty() {
            self.meeting_system_frames
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .splice(0..0, system);
        }
    }

    /// The capture gate: this layer admits exactly one recorder.
    ///
    /// Dictation, meetings and the input preview all open a `cpal` stream on
    /// the same input device, and two streams on one device is a second orange
    /// microphone indicator and two sets of buffers nobody asked for. Every
    /// entry point asks here first, so the conflict is refused with a sentence
    /// the user can act on instead of discovered later.
    fn capture_gate(&self, who: Capture) -> Result<(), AudioIoError> {
        let holder = if *self
            .dictation_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            != DictationState::Idle
        {
            Some(Capture::Dictation)
        } else if *self.meeting_state.lock().unwrap_or_else(|p| p.into_inner())
            != MeetingState::Idle
        {
            Some(Capture::Meeting)
        } else if self
            .preview_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            Some(Capture::Preview)
        } else {
            None
        };

        match holder {
            None => Ok(()),
            Some(holder) if holder == who => Err(AudioIoError::Other(format!(
                "{} is already running",
                who.label()
            ))),
            Some(holder) => Err(AudioIoError::Other(format!(
                "{} is using the microphone",
                holder.label()
            ))),
        }
    }

    /// The preferred device resolved against the current enumeration, with any
    /// fallback recorded for the app to report.
    fn open_input_device(&self, host: &cpal::Host) -> Result<Device, AudioIoError> {
        let preferred = self
            .preferred_input_device
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let (device, fallback) = resolve_input_device(host, preferred.as_deref())?;
        // Recorded once here, at resolution, rather than from the callback:
        // the user needs telling once per recording, not per audio buffer.
        *self
            .device_fallback
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = fallback;
        if let Ok(config) = device.default_input_config() {
            *self
                .sample_rate_hz
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = config.sample_rate().0;
        }
        Ok(device)
    }

    /// Stop the preview stream if one is running, reporting whether there was.
    fn stop_preview_worker(&self) -> bool {
        let worker = self
            .preview_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        match worker {
            Some(worker) => {
                if let Err(err) = stop_worker(worker) {
                    tracing::warn!("input preview did not stop cleanly: {err}");
                }
                *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;
                true
            }
            None => false,
        }
    }
}

/// Who wants the one capture device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capture {
    Dictation,
    Meeting,
    Preview,
}

impl Capture {
    fn label(self) -> &'static str {
        match self {
            Capture::Dictation => "dictation",
            Capture::Meeting => "meeting capture",
            Capture::Preview => "the microphone test",
        }
    }
}

/// Hand an armed stream over to a recording: drain the preroll ahead of the
/// live audio, then point the callback at the dictation sink.
///
/// Both happen under one lock, so a buffer arriving mid-handover cannot land
/// in front of the preroll it is supposed to follow.
fn adopt_armed_capture(armed: ArmedCapture, sink: DictationSink) -> CaptureWorker {
    {
        let mut state = armed.state.lock().unwrap_or_else(|p| p.into_inner());
        let preroll = state.ring.drain();
        if !preroll.is_empty() {
            let frame = PcmFrame {
                samples: preroll,
                sample_rate_hz: state.sample_rate_hz,
            };
            sink.counters.send(&sink.tx, frame.clone(), "dictation");
            sink.buffered
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(frame);
        }
        state.recording = Some(sink);
    }
    armed.worker
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

/// How much audio the preroll ring holds.
///
/// It has to cover the whole window between the stream going live and the hold
/// threshold firing: [`crate::hotkeys::hold::ARM_DELAY`] on the first modifier,
/// plus [`crate::hotkeys::hold::DEFAULT_MIN_HOLD`] (350ms) once the chord
/// completes. 600ms clears that with room for a slow chord, and costs 38 KB at
/// 16 kHz mono f32 — small enough that trimming it would buy nothing.
const PREROLL_MS: u32 = 600;

/// Every input device the host can enumerate, with the default marked.
fn list_devices(host: &cpal::Host) -> Vec<InputDevice> {
    let default_name = host.default_input_device().and_then(|d| d.name().ok());
    host.input_devices()
        .into_iter()
        .flatten()
        .filter_map(|device| {
            let name = device.name().ok()?;
            Some(InputDevice {
                is_default: Some(&name) == default_name.as_ref(),
                id: name.clone(),
                name,
            })
        })
        .collect()
}

/// Open the device the user asked for, or the default when they asked for
/// nothing — or when what they asked for is gone.
///
/// Returns the fallback alongside the device so the caller can report it once,
/// at resolution time, rather than from inside an audio callback.
fn resolve_input_device(
    host: &cpal::Host,
    preferred: Option<&str>,
) -> Result<(Device, Option<DeviceFallback>), AudioIoError> {
    let choice = match preferred {
        // The common case by far; skip enumerating the host for it.
        None => DeviceChoice::Default,
        Some(_) => choose_input_device(&list_devices(host), preferred),
    };

    let named = match &choice {
        DeviceChoice::Preferred(name) => host
            .input_devices()
            .into_iter()
            .flatten()
            .find(|d| d.name().is_ok_and(|n| &n == name)),
        _ => None,
    };

    let fallback = match choice {
        DeviceChoice::Fallback(fallback) => {
            tracing::warn!(
                requested = %fallback.requested,
                using = fallback.using.as_deref().unwrap_or("<the default input>"),
                "the saved input device is not connected; recording from the default instead"
            );
            Some(fallback)
        }
        _ => None,
    };

    let device = match named {
        Some(device) => device,
        None => host
            .default_input_device()
            .ok_or_else(|| AudioIoError::Other("no input device".into()))?,
    };
    Ok((device, fallback))
}

fn default_input_sample_rate() -> Option<u32> {
    let host = cpal::default_host();
    let device = host.default_input_device()?;
    let config = device.default_input_config().ok()?;
    Some(config.sample_rate().0)
}

/// Where an armed stream sends audio once the hold has become a recording.
struct DictationSink {
    tx: tokio::sync::mpsc::Sender<PcmFrame>,
    buffered: Arc<Mutex<Vec<PcmFrame>>>,
    counters: FrameCounters,
}

/// The armed stream's shared state, written from the audio callback and
/// switched over by `start_mic` when the threshold passes.
struct ArmedState {
    ring: RingBuffer,
    /// The stream's own rate, learnt from the first callback. The preroll has
    /// to be handed back at the rate it was captured at.
    sample_rate_hz: u32,
    /// `None` while merely armed; `Some` once this stream is the recording.
    recording: Option<DictationSink>,
}

/// A capture stream opened before the recording it may become.
///
/// `start_mic` adopts this rather than opening its own: the probe in
/// `examples/mic_arm_probe.rs` measures ~120-150ms from `build_input_stream` to
/// the first buffer, so re-opening at the threshold would throw away exactly
/// the audio arming was meant to keep.
struct ArmedCapture {
    state: Arc<Mutex<ArmedState>>,
    worker: CaptureWorker,
}

fn run_armed_capture(
    device: Device,
    stop_rx: mpsc::Receiver<()>,
    state: Arc<Mutex<ArmedState>>,
    level: Arc<Mutex<f32>>,
) -> Result<(), AudioIoError> {
    run_input_stream(device, stop_rx, move |mono, sample_rate_hz| {
        let mut armed = state.lock().unwrap_or_else(|p| p.into_inner());
        armed.sample_rate_hz = sample_rate_hz;
        match armed.recording.as_ref() {
            // Adopted: this is an ordinary dictation capture now, and takes the
            // same path every other frame does.
            Some(sink) => push_dictation_frame(
                mono,
                sample_rate_hz,
                &level,
                &sink.tx,
                &sink.buffered,
                &sink.counters,
            ),
            None => {
                *level.lock().unwrap_or_else(|p| p.into_inner()) = rms_level(&mono);
                armed.ring.push(&mono);
            }
        }
    })
}

/// Publish the input level and discard the audio: what "Test microphone" needs
/// and the most a preview is ever allowed to do with the samples.
fn run_preview_capture(
    device: Device,
    stop_rx: mpsc::Receiver<()>,
    level: Arc<Mutex<f32>>,
) -> Result<(), AudioIoError> {
    run_input_stream(device, stop_rx, move |mono, _sample_rate_hz| {
        *level.lock().unwrap_or_else(|p| p.into_inner()) = rms_level(&mono);
    })
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
    counters: &FrameCounters,
) {
    let frame = PcmFrame {
        samples,
        sample_rate_hz,
    };
    *level.lock().unwrap_or_else(|p| p.into_inner()) = rms_level(&frame.samples);
    // Ignored on purpose: a frame that does not reach a streaming consumer is
    // still recorded, because the session buffer below takes every frame.
    counters.send(tx, frame.clone(), "dictation");
    buffered
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(frame);
}

/// The first `take` samples of `frame`.
fn head(frame: &PcmFrame, take: usize) -> PcmFrame {
    PcmFrame {
        samples: frame.samples[..take.min(frame.samples.len())].to_vec(),
        sample_rate_hz: frame.sample_rate_hz,
    }
}

/// Everything after the first `take` samples of `frame`.
fn tail(frame: &PcmFrame, take: usize) -> PcmFrame {
    PcmFrame {
        samples: frame.samples[take.min(frame.samples.len())..].to_vec(),
        sample_rate_hz: frame.sample_rate_hz,
    }
}

/// A drain buffer holding `frame`, or empty when there is nothing left — an
/// empty buffer is how "this source is not recording" is spelled, so a
/// zero-sample frame must never be stored.
fn refill(frame: PcmFrame) -> Vec<PcmFrame> {
    if frame.samples.is_empty() {
        Vec::new()
    } else {
        vec![frame]
    }
}

/// Buffer one mic frame for a meeting. Deliberately does *not* mix: mixing
/// here is what dropped or duplicated system audio, because a mic callback can
/// see zero or two loopback frames as easily as one.
fn push_meeting_frame(
    frame: PcmFrame,
    level: &Arc<Mutex<f32>>,
    system_level: &Arc<Mutex<f32>>,
    tx: &tokio::sync::mpsc::Sender<PcmFrame>,
    drain_frames: &Arc<Mutex<Vec<PcmFrame>>>,
    counters: &FrameCounters,
) {
    // The published level is the louder of the two sources rather than the
    // mix's: the meter answers "is anything being heard", and averaging in a
    // silent channel halves the needle while the other side is talking.
    let mic_level = rms_level(&frame.samples);
    let system_level = *system_level.lock().unwrap_or_else(|p| p.into_inner());
    *level.lock().unwrap_or_else(|p| p.into_inner()) = mic_level.max(system_level);

    counters.send(tx, frame.clone(), "meeting");
    drain_frames
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .push(frame);
}

/// Append a system frame, rather than overwrite a single latest-frame slot.
///
/// The slot was the bug: two loopback callbacks between two mic callbacks lost
/// one frame outright, and zero of them mixed the previous frame in twice.
fn push_system_frame(
    frame: PcmFrame,
    system_level: &Arc<Mutex<f32>>,
    frames: &Arc<Mutex<Vec<PcmFrame>>>,
) {
    *system_level.lock().unwrap_or_else(|p| p.into_inner()) = rms_level(&frame.samples);
    frames.lock().unwrap_or_else(|p| p.into_inner()).push(frame);
}

fn run_capture_on_device(
    device: Device,
    stop_rx: mpsc::Receiver<()>,
    frame_tx: tokio::sync::mpsc::Sender<PcmFrame>,
    dictation_buffered: Arc<Mutex<Vec<PcmFrame>>>,
    level: Arc<Mutex<f32>>,
    counters: FrameCounters,
) -> Result<(), AudioIoError> {
    run_input_stream(device, stop_rx, move |mono, sample_rate_hz| {
        push_dictation_frame(
            mono,
            sample_rate_hz,
            &level,
            &frame_tx,
            &dictation_buffered,
            &counters,
        );
    })
}

fn run_meeting_mic_capture(
    device: Device,
    stop_rx: mpsc::Receiver<()>,
    frame_tx: tokio::sync::mpsc::Sender<PcmFrame>,
    drain_frames: Arc<Mutex<Vec<PcmFrame>>>,
    level: Arc<Mutex<f32>>,
    system_level: Arc<Mutex<f32>>,
    counters: FrameCounters,
) -> Result<(), AudioIoError> {
    run_input_stream(device, stop_rx, move |mono, sample_rate_hz| {
        let frame = PcmFrame {
            samples: mono,
            sample_rate_hz,
        };
        push_meeting_frame(
            frame,
            &level,
            &system_level,
            &frame_tx,
            &drain_frames,
            &counters,
        );
    })
}

/// Start system-audio capture and append its frames to the system drain
/// buffer. The capture stays owned by [`MacAudioIo`] so `stop_meeting` can
/// stop the same instance.
async fn start_system_audio(
    capture: &mut dyn SystemAudioCapture,
    system_level: Arc<Mutex<f32>>,
    frames: Arc<Mutex<Vec<PcmFrame>>>,
) -> Result<SckMeetingWorker, AudioIoError> {
    let mut frame_rx = capture.start().await?;
    let pump = thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime for SCK pump");
        rt.block_on(async move {
            while let Some(frame) = frame_rx.recv().await {
                push_system_frame(frame, &system_level, &frames);
            }
        });
    });
    Ok(SckMeetingWorker { pump })
}

fn run_loopback_capture(
    device: Device,
    stop_rx: mpsc::Receiver<()>,
    system_level: Arc<Mutex<f32>>,
    frames: Arc<Mutex<Vec<PcmFrame>>>,
) -> Result<(), AudioIoError> {
    run_input_stream(device, stop_rx, move |mono, sample_rate_hz| {
        push_system_frame(
            PcmFrame {
                samples: mono,
                sample_rate_hz,
            },
            &system_level,
            &frames,
        );
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
        // A running preview loses to a recording rather than blocking it: the
        // user who pressed the hotkey has said what they want, and the preview
        // is only ever a diagnostic. Done before the gate so the gate sees the
        // device free.
        if self.stop_preview_worker() {
            tracing::debug!("input preview cancelled: dictation is starting");
        }
        self.capture_gate(Capture::Dictation)?;

        self.dictation_buffered
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;

        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(64);
        let counters = self.dictation_frames.clone();
        counters.reset();
        let buffered = Arc::clone(&self.dictation_buffered);

        let armed = self
            .armed_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();

        let worker = match armed {
            // The stream is already live and the preroll already holds the
            // speech from before the threshold. Opening a second one here
            // would throw that away and pay the open latency again.
            Some(armed) => adopt_armed_capture(
                armed,
                DictationSink {
                    tx: frame_tx,
                    buffered,
                    counters,
                },
            ),
            None => {
                let device = self.open_input_device(&cpal::default_host())?;
                let (stop_tx, stop_rx) = mpsc::channel();
                let level = Arc::clone(&self.level);
                let join = thread::spawn(move || {
                    if let Err(err) =
                        run_capture_on_device(device, stop_rx, frame_tx, buffered, level, counters)
                    {
                        tracing::error!("mic capture failed: {err}");
                    }
                });
                CaptureWorker { stop_tx, join }
            }
        };

        *self
            .dictation_state
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = DictationState::Listening;
        *self
            .dictation_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(worker);

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

        self.dictation_frames.log_session("dictation");

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
        if self.stop_preview_worker() {
            tracing::debug!("input preview cancelled: a meeting is starting");
        }
        self.capture_gate(Capture::Meeting)?;

        self.meeting_drain_frames
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        self.meeting_system_frames
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;
        *self.system_level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;

        let host = cpal::default_host();
        let mic_device = self.open_input_device(&host)?;

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

        let (frame_tx, frame_rx) = tokio::sync::mpsc::channel(64);
        let (mic_stop_tx, mic_stop_rx) = mpsc::channel();
        let drain_frames = Arc::clone(&self.meeting_drain_frames);
        let level = Arc::clone(&self.level);
        let system_level = Arc::clone(&self.system_level);
        let counters = self.meeting_mic_frames.clone();
        counters.reset();

        let mic_join = thread::spawn(move || {
            if let Err(err) = run_meeting_mic_capture(
                mic_device,
                mic_stop_rx,
                frame_tx,
                drain_frames,
                level,
                system_level,
                counters,
            ) {
                tracing::error!("meeting mic capture failed: {err}");
            }
        });

        let loopback_worker = if let Some(device) = loopback_device {
            let (loop_stop_tx, loop_stop_rx) = mpsc::channel();
            let system_level = Arc::clone(&self.system_level);
            let system_frames = Arc::clone(&self.meeting_system_frames);
            let join = thread::spawn(move || {
                if let Err(err) =
                    run_loopback_capture(device, loop_stop_rx, system_level, system_frames)
                {
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
            Some(
                start_system_audio(
                    self.system_audio.as_mut(),
                    Arc::clone(&self.system_level),
                    Arc::clone(&self.meeting_system_frames),
                )
                .await?,
            )
        } else {
            None
        };

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
        self.meeting_system_frames
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        *self.meeting_state.lock().unwrap_or_else(|p| p.into_inner()) = MeetingState::Idle;
        *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;
        *self.system_level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;

        self.meeting_mic_frames.log_session("meeting");

        Ok(PcmFrame {
            samples: vec![],
            sample_rate_hz: *self
                .sample_rate_hz
                .lock()
                .unwrap_or_else(|p| p.into_inner()),
        })
    }

    async fn drain_meeting_sources(&mut self) -> Result<crate::audio::SpeechSegment, AudioIoError> {
        let (mic_frames, system_frames) = self.take_pending_frames();
        let had_system = !system_frames.is_empty();
        let sources = align_meeting_sources(
            &accumulate_frames(&mic_frames),
            &accumulate_frames(&system_frames),
        );
        Ok(crate::audio::SpeechSegment {
            // The tail is taken because the meeting is over, not because a
            // pause was found, so there is no cut decision to report. The
            // caller gates it on duration instead.
            has_speech: true,
            pcm: sources.mixed,
            mic: had_system.then_some(sources.mic),
            system: had_system.then_some(sources.system),
        })
    }

    async fn try_drain_meeting_segment(
        &mut self,
        cfg: crate::audio::segment::SegmentCutConfig,
    ) -> Result<Option<crate::audio::SpeechSegment>, AudioIoError> {
        // Both buffers are emptied back to back, so what was taken ends at the
        // same instant on each side — which is what `align_meeting_sources`
        // relies on.
        let (mic_frames, system_frames) = self.take_pending_frames();

        // An empty system buffer is the liveness test for the far side: a
        // running loopback stream delivers frames continuously, silence
        // included, so nothing at all means no system source this meeting (or
        // one that has stopped) rather than a quiet call.
        let had_system = !system_frames.is_empty();
        let sources = align_meeting_sources(
            &accumulate_frames(&mic_frames),
            &accumulate_frames(&system_frames),
        );

        // The cut is decided on the mix — everything that was said — and then
        // applied to both halves at the same sample index, which is what keeps
        // them comparable window for window downstream.
        let Some(cut) = crate::audio::segment::find_segment_cut(
            &sources.mixed.samples,
            sources.mixed.sample_rate_hz,
            cfg,
        ) else {
            // Not at a cut point yet. The frames go back exactly as they were
            // taken rather than as the aligned pair: the padding above is
            // derived, recomputed from the raw buffers on every poll, and
            // storing it would bake one poll's skew into the rest of the
            // meeting.
            self.restore_pending_frames(mic_frames, system_frames);
            return Ok(None);
        };

        // Keep whatever follows the cut as the start of the next segment, so
        // audio spoken after the pause is not discarded. The remainder goes
        // back aligned — both sides cut at one index — so the next segment
        // starts from the same instant on each.
        self.restore_pending_frames(
            refill(tail(&sources.mic, cut.take)),
            if had_system {
                refill(tail(&sources.system, cut.take))
            } else {
                Vec::new()
            },
        );

        Ok(Some(crate::audio::SpeechSegment {
            pcm: head(&sources.mixed, cut.take),
            has_speech: cut.has_speech,
            // No system source means nothing to attribute against; the caller
            // must not read silence as "the far side said nothing".
            mic: had_system.then(|| head(&sources.mic, cut.take)),
            system: had_system.then(|| head(&sources.system, cut.take)),
        }))
    }

    fn list_input_devices(&self) -> Vec<InputDevice> {
        list_devices(&cpal::default_host())
    }

    fn set_input_device(&mut self, preferred: Option<String>) {
        *self
            .preferred_input_device
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = preferred;
    }

    fn take_device_fallback(&mut self) -> Option<DeviceFallback> {
        self.device_fallback
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }

    fn preview_active(&self) -> bool {
        self.preview_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }

    async fn start_input_preview(&mut self) -> Result<(), AudioIoError> {
        self.capture_gate(Capture::Preview)?;
        // An armed stream is holding the device for a chord that has not
        // become a recording. The preview is a deliberate user action, so it
        // wins — the next chord re-arms.
        self.disarm_capture();

        let device = self.open_input_device(&cpal::default_host())?;
        let (stop_tx, stop_rx) = mpsc::channel();
        let level = Arc::clone(&self.level);
        *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;
        let join = thread::spawn(move || {
            if let Err(err) = run_preview_capture(device, stop_rx, level) {
                tracing::error!("input preview failed: {err}");
            }
        });
        *self
            .preview_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(CaptureWorker { stop_tx, join });
        Ok(())
    }

    async fn stop_input_preview(&mut self) -> Result<(), AudioIoError> {
        self.stop_preview_worker();
        Ok(())
    }

    fn arm_capture(&mut self) {
        if self
            .armed_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
        {
            return;
        }
        // Arming is an optimisation on a keypress, so a busy device is not an
        // error here: the recording that follows will open its own stream and
        // simply start ~150ms later, which is today's behaviour.
        if let Err(err) = self.capture_gate(Capture::Dictation) {
            tracing::debug!("not arming the preroll capture: {err}");
            return;
        }

        let device = match self.open_input_device(&cpal::default_host()) {
            Ok(device) => device,
            Err(err) => {
                tracing::debug!("not arming the preroll capture: {err}");
                return;
            }
        };
        let capacity = (*self
            .sample_rate_hz
            .lock()
            .unwrap_or_else(|p| p.into_inner()) as usize)
            * PREROLL_MS as usize
            / 1000;

        let state = Arc::new(Mutex::new(ArmedState {
            ring: RingBuffer::with_capacity(capacity),
            sample_rate_hz: 0,
            recording: None,
        }));
        let (stop_tx, stop_rx) = mpsc::channel();
        let level = Arc::clone(&self.level);
        let for_thread = Arc::clone(&state);
        let join = thread::spawn(move || {
            if let Err(err) = run_armed_capture(device, stop_rx, for_thread, level) {
                tracing::error!("preroll capture failed: {err}");
            }
        });

        *self.armed_capture.lock().unwrap_or_else(|p| p.into_inner()) = Some(ArmedCapture {
            state,
            worker: CaptureWorker { stop_tx, join },
        });
    }

    fn disarm_capture(&mut self) {
        let armed = self
            .armed_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take();
        // The ring goes with the worker: audio captured for a chord that never
        // became a recording is never transcribed and never stored.
        if let Some(armed) = armed {
            if let Err(err) = stop_worker(armed.worker) {
                tracing::warn!("preroll capture did not stop cleanly: {err}");
            }
            *self.level.lock().unwrap_or_else(|p| p.into_inner()) = 0.0;
        }
    }

    fn is_armed(&self) -> bool {
        self.armed_capture
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
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
    async fn system_audio_pump_appends_frames_to_the_system_buffer() {
        let mut capture = FakeSck;
        let frames = Arc::new(Mutex::new(Vec::new()));
        let level = Arc::new(Mutex::new(0.0));
        let worker = start_system_audio(&mut capture, Arc::clone(&level), Arc::clone(&frames))
            .await
            .expect("fake backend starts");
        // The fake closes its channel after one frame, so the pump exits on its own.
        worker.pump.join().expect("pump thread");
        let buffered = frames.lock().unwrap();
        assert_eq!(buffered.len(), 1);
        assert_eq!(buffered[0].samples.len(), 100);
        assert_eq!(buffered[0].sample_rate_hz, 48_000);
    }

    /// A capture worker that holds its slot and nothing else. Lets the gate
    /// tests put the preview in the "running" state without opening a stream —
    /// these tests must never touch the real microphone.
    fn idle_worker() -> CaptureWorker {
        let (stop_tx, stop_rx) = mpsc::channel();
        let join = thread::spawn(move || {
            let _ = stop_rx.recv();
        });
        CaptureWorker { stop_tx, join }
    }

    #[test]
    fn the_capture_gate_names_whoever_holds_the_device() {
        let io = MacAudioIo::new_for_test();
        assert!(io.capture_gate(Capture::Preview).is_ok());

        *io.dictation_state.lock().unwrap() = DictationState::Listening;
        let err = io.capture_gate(Capture::Preview).unwrap_err().to_string();
        assert!(err.contains("dictation"), "{err}");
        let err = io.capture_gate(Capture::Dictation).unwrap_err().to_string();
        assert!(err.contains("already running"), "{err}");

        *io.dictation_state.lock().unwrap() = DictationState::Idle;
        *io.meeting_state.lock().unwrap() = MeetingState::Recording;
        let err = io.capture_gate(Capture::Dictation).unwrap_err().to_string();
        assert!(err.contains("meeting"), "{err}");
    }

    #[test]
    fn a_running_preview_holds_the_gate_against_a_recording() {
        let io = MacAudioIo::new_for_test();
        *io.preview_capture.lock().unwrap() = Some(idle_worker());
        assert!(io.preview_active());

        let err = io.capture_gate(Capture::Dictation).unwrap_err().to_string();
        assert!(err.contains("microphone test"), "{err}");

        assert!(io.stop_preview_worker(), "the preview was running");
        assert!(!io.preview_active());
        assert!(io.capture_gate(Capture::Dictation).is_ok());
        assert!(!io.stop_preview_worker(), "stopping twice is not an error");
    }

    #[tokio::test]
    async fn a_dictation_start_cancels_the_preview_before_it_asks_the_gate() {
        // A preview left running when the hotkey fires would be a second cpal
        // stream on one input device. The meeting flag is forced on so the
        // gate refuses *after* the cancel, which is how this asserts the
        // ordering without opening a real stream.
        let mut io = MacAudioIo::new_for_test();
        *io.preview_capture.lock().unwrap() = Some(idle_worker());
        *io.meeting_state.lock().unwrap() = MeetingState::Recording;

        assert!(io.start_mic().await.is_err(), "the meeting still wins");
        assert!(
            !io.preview_active(),
            "the preview must be released whether or not the recording starts"
        );
    }

    #[test]
    fn a_device_fallback_is_reported_once_per_resolution() {
        // Resolution happens once per stream open; the audio callback runs
        // hundreds of times a second, and must not produce a notification each.
        let mut io = MacAudioIo::new_for_test();
        *io.device_fallback.lock().unwrap() = Some(DeviceFallback {
            requested: "Yeti".into(),
            using: Some("MacBook Air Microphone".into()),
        });
        assert_eq!(
            io.take_device_fallback().map(|f| f.requested),
            Some("Yeti".into())
        );
        assert!(io.take_device_fallback().is_none());
    }

    #[test]
    fn a_saved_device_is_remembered_for_the_next_stream_open() {
        let mut io = MacAudioIo::new_for_test();
        io.set_input_device(Some("Yeti".into()));
        assert_eq!(
            io.preferred_input_device.lock().unwrap().as_deref(),
            Some("Yeti")
        );
        io.set_input_device(None);
        assert!(io.preferred_input_device.lock().unwrap().is_none());
    }

    #[test]
    fn arming_is_refused_while_something_else_holds_the_device() {
        // Arming is an optimisation on a keypress: a busy device means no
        // preroll, never a second stream and never an error the user sees.
        let mut io = MacAudioIo::new_for_test();
        *io.meeting_state.lock().unwrap() = MeetingState::Recording;
        io.arm_capture();
        assert!(!io.is_armed());
        io.disarm_capture();
        assert!(!io.is_armed(), "disarming what was never armed is a no-op");
    }

    #[tokio::test]
    async fn unavailable_system_audio_refuses_to_start() {
        let mut capture = UnavailableSystemAudioCapture;
        let Err(AudioIoError::Other(msg)) = start_system_audio(
            &mut capture,
            Arc::new(Mutex::new(0.0)),
            Arc::new(Mutex::new(Vec::new())),
        )
        .await
        else {
            panic!("null object must refuse to start");
        };
        assert!(msg.contains("system-audio-sck"));
    }

    fn speech(secs: f32, rate: u32, amplitude: f32) -> PcmFrame {
        let n = (secs * rate as f32) as usize;
        PcmFrame {
            samples: (0..n)
                .map(|i| (i as f32 * 0.05).sin() * amplitude)
                .collect(),
            sample_rate_hz: rate,
        }
    }

    /// A cut is decided on the mix but applied to both halves at one index, so
    /// the two channels the attributor compares describe the same instants.
    #[tokio::test]
    async fn a_meeting_segment_carries_both_sources_cut_at_one_index() {
        let mut io = MacAudioIo::new_for_test();
        // The loopback stream opens after the mic's, so it is short by design.
        io.meeting_drain_frames
            .lock()
            .unwrap()
            .push(speech(6.0, 16_000, 0.3));
        io.meeting_system_frames
            .lock()
            .unwrap()
            .push(speech(5.0, 16_000, 0.02));

        let cfg = crate::audio::segment::SegmentCutConfig {
            max_secs: 5.0,
            ..Default::default()
        };
        let segment = io
            .try_drain_meeting_segment(cfg)
            .await
            .unwrap()
            .expect("6s of audio is past the 5s cap");

        let mic = segment.mic.expect("a system source was recorded");
        let system = segment.system.expect("a system source was recorded");
        assert_eq!(mic.samples.len(), system.samples.len());
        assert_eq!(mic.samples.len(), segment.pcm.samples.len());
        assert_eq!(mic.sample_rate_hz, system.sample_rate_hz);
        // Mixed is the average of the two halves, sample for sample.
        assert!((segment.pcm.samples[0] - (mic.samples[0] + system.samples[0]) / 2.0).abs() < 1e-6);
        // The whole buffer was taken at the cap, so nothing is left over.
        assert!(io.meeting_drain_frames.lock().unwrap().is_empty());
        assert!(io.meeting_system_frames.lock().unwrap().is_empty());
    }

    /// Mic-only: there is no second channel, and inventing a silent one would
    /// make every segment look mic-dominant rather than unattributable.
    #[tokio::test]
    async fn a_mic_only_meeting_segment_has_no_per_source_halves() {
        let mut io = MacAudioIo::new_for_test();
        io.meeting_drain_frames
            .lock()
            .unwrap()
            .push(speech(6.0, 16_000, 0.3));

        let cfg = crate::audio::segment::SegmentCutConfig {
            max_secs: 5.0,
            ..Default::default()
        };
        let segment = io.try_drain_meeting_segment(cfg).await.unwrap().unwrap();
        assert!(segment.mic.is_none());
        assert!(segment.system.is_none());
        assert_eq!(segment.pcm.samples.len(), 6 * 16_000);
    }

    /// Before a cut point both buffers stay exactly as they were: the padding
    /// that aligns them is derived per poll, never written back.
    #[tokio::test]
    async fn a_poll_that_finds_no_cut_leaves_both_buffers_untouched() {
        let mut io = MacAudioIo::new_for_test();
        io.meeting_drain_frames
            .lock()
            .unwrap()
            .push(speech(1.0, 16_000, 0.3));
        io.meeting_system_frames
            .lock()
            .unwrap()
            .push(speech(0.5, 16_000, 0.3));

        let cfg = crate::audio::segment::SegmentCutConfig::default();
        assert!(io.try_drain_meeting_segment(cfg).await.unwrap().is_none());
        assert_eq!(
            io.meeting_drain_frames.lock().unwrap()[0].samples.len(),
            16_000
        );
        assert_eq!(
            io.meeting_system_frames.lock().unwrap()[0].samples.len(),
            8_000
        );
    }

    /// The remainder after a cut is kept on both sides and stays aligned, so
    /// the next segment starts from one index on both channels.
    #[tokio::test]
    async fn the_remainder_after_a_cut_stays_aligned_on_both_sources() {
        let mut io = MacAudioIo::new_for_test();
        let mut mic = speech(8.0, 16_000, 0.3).samples;
        mic.extend(std::iter::repeat_n(0.0, 4 * 16_000));
        io.meeting_drain_frames.lock().unwrap().push(PcmFrame {
            samples: mic,
            sample_rate_hz: 16_000,
        });
        io.meeting_system_frames
            .lock()
            .unwrap()
            .push(speech(12.0, 16_000, 0.001));

        let cfg = crate::audio::segment::SegmentCutConfig::default();
        let segment = io
            .try_drain_meeting_segment(cfg)
            .await
            .unwrap()
            .expect("3.5s of trailing silence is a pause");
        assert!(
            segment.pcm.samples.len() < 12 * 16_000,
            "cut before the end"
        );

        let mic_left = io.meeting_drain_frames.lock().unwrap()[0].samples.len();
        let system_left = io.meeting_system_frames.lock().unwrap()[0].samples.len();
        assert_eq!(mic_left, system_left);
        assert_eq!(mic_left + segment.pcm.samples.len(), 12 * 16_000);
    }

    /// The tail taken at stop is a segment like any other, so the last thing
    /// said in a meeting is attributed rather than left blank.
    #[tokio::test]
    async fn the_stop_drain_returns_both_sources_too() {
        let mut io = MacAudioIo::new_for_test();
        io.meeting_drain_frames
            .lock()
            .unwrap()
            .push(speech(2.0, 16_000, 0.3));
        io.meeting_system_frames
            .lock()
            .unwrap()
            .push(speech(2.0, 16_000, 0.3));

        let tail = io.drain_meeting_sources().await.unwrap();
        assert_eq!(tail.pcm.samples.len(), 2 * 16_000);
        assert_eq!(tail.mic.unwrap().samples.len(), 2 * 16_000);
        assert_eq!(tail.system.unwrap().samples.len(), 2 * 16_000);
        assert!(io.meeting_system_frames.lock().unwrap().is_empty());
    }

    /// Two system frames between two mic callbacks used to overwrite one
    /// another in a single-frame slot; now both survive to the drain.
    #[test]
    fn every_system_frame_is_kept_rather_than_overwriting_the_last() {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let level = Arc::new(Mutex::new(0.0));
        for value in [0.1f32, 0.2, 0.3] {
            push_system_frame(
                PcmFrame {
                    samples: vec![value; 4],
                    sample_rate_hz: 16_000,
                },
                &level,
                &frames,
            );
        }
        let kept = accumulate_frames(&frames.lock().unwrap());
        assert_eq!(kept.samples.len(), 12);
        assert_eq!(kept.samples[0], 0.1);
        assert_eq!(kept.samples[11], 0.3);
    }
}
