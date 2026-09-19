//! Pure audio helpers (resample, RMS, frame accumulation) plus the frame-sink
//! bookkeeping shared by the capture callbacks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::Sender;

use super::{DeviceFallback, InputDevice, PcmFrame};

/// Delivery counters for a bounded capture channel, split by *why* a frame did
/// not reach the streaming consumer.
///
/// `try_send` reports "the consumer is behind" (`Full`) and "there is no
/// consumer" (`Closed`) as the same `Err`, and collapsing the two is a bug with
/// a visible symptom: the production caller of `start_mic` drops the receiver
/// (`src-tauri/src/commands.rs`, which discards the `Ok` value), so the channel
/// is *closed* on every dictation run and the logs blamed a full channel for it.
///
/// The distinction is not cosmetic. A `Full` is real loss for whoever is
/// streaming. A `Closed` loses nothing at all — the capture callback also writes
/// every frame into the session buffer that `stop_mic` returns, so the recording
/// is complete either way. Only the first is worth a warning.
#[derive(Clone, Debug, Default)]
pub struct FrameCounters {
    full: Arc<AtomicU64>,
    closed: Arc<AtomicU64>,
}

impl FrameCounters {
    pub fn new() -> Self {
        Self::default()
    }

    /// Zero both counters at the start of a capture session.
    pub fn reset(&self) {
        self.full.store(0, Ordering::Relaxed);
        self.closed.store(0, Ordering::Relaxed);
    }

    /// `(full, closed)` since the last [`reset`](Self::reset) or
    /// [`log_session`](Self::log_session).
    pub fn totals(&self) -> (u64, u64) {
        (
            self.full.load(Ordering::Relaxed),
            self.closed.load(Ordering::Relaxed),
        )
    }

    /// Offer `frame` to the streaming consumer, recording why it did not land.
    ///
    /// Returns whether the frame reached the channel. `false` is not an error at
    /// this layer and callers are expected to ignore it: the session buffer is
    /// written separately and is what the recording is actually built from.
    ///
    /// Runs inside the cpal callback, so it must not block — hence `try_send`
    /// and relaxed counters rather than any form of waiting.
    pub fn send(&self, tx: &Sender<PcmFrame>, frame: PcmFrame, label: &str) -> bool {
        match tx.try_send(frame) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) => {
                let n = self.full.fetch_add(1, Ordering::Relaxed) + 1;
                // Every 100th, so a sustained overrun stays visible without
                // one log line per 10ms of audio.
                if n.is_multiple_of(100) {
                    tracing::warn!(
                        full_drops = n,
                        "{}: streaming consumer is behind; dropped {} mic frames so far \
                         (the buffered recording is unaffected)",
                        label,
                        n
                    );
                }
                false
            }
            Err(TrySendError::Closed(_)) => {
                let n = self.closed.fetch_add(1, Ordering::Relaxed) + 1;
                // Once per session rather than every 100 frames: with no
                // receiver attached this is the steady state, not a fault.
                if n == 1 {
                    tracing::debug!(
                        "{}: no streaming consumer attached; frames are buffered only",
                        label
                    );
                }
                false
            }
        }
    }

    /// Drain and report the session totals. Only a `Full` count is a warning.
    pub fn log_session(&self, label: &str) {
        let full = self.full.swap(0, Ordering::Relaxed);
        let closed = self.closed.swap(0, Ordering::Relaxed);
        if full > 0 {
            tracing::warn!(
                full_drops = full,
                "{}: dropped {} mic frames this session because the streaming consumer \
                 fell behind",
                label,
                full
            );
        }
        if closed > 0 {
            tracing::debug!(
                closed,
                "{}: {} frames had no streaming consumer this session",
                label,
                closed
            );
        }
    }
}

/// Linearly resample `frame` to `target_rate_hz`.
pub fn resample_linear(frame: &PcmFrame, target_rate_hz: u32) -> PcmFrame {
    if frame.sample_rate_hz == 0 || target_rate_hz == 0 || frame.samples.is_empty() {
        return PcmFrame {
            samples: Vec::new(),
            sample_rate_hz: target_rate_hz,
        };
    }

    if frame.sample_rate_hz == target_rate_hz {
        return frame.clone();
    }

    let ratio = target_rate_hz as f64 / frame.sample_rate_hz as f64;
    let out_len = ((frame.samples.len() as f64) * ratio).round() as usize;
    if out_len == 0 {
        return PcmFrame {
            samples: Vec::new(),
            sample_rate_hz: target_rate_hz,
        };
    }

    let mut out = Vec::with_capacity(out_len);
    let max_idx = frame.samples.len().saturating_sub(1);

    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = src_pos - idx as f64;
        let a = frame.samples[idx.min(max_idx)];
        let b = frame.samples[(idx + 1).min(max_idx)];
        out.push(a + (b - a) * frac as f32);
    }

    PcmFrame {
        samples: out,
        sample_rate_hz: target_rate_hz,
    }
}

/// RMS level in \[0.0, 1.0\] for UI metering.
pub fn rms_level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    let rms = (sum_sq / samples.len() as f32).sqrt();
    rms.clamp(0.0, 1.0)
}

/// Mix two mono frames to mono (resample to the higher rate, average aligned samples).
pub fn mix_frames(mic: &PcmFrame, system: &PcmFrame) -> PcmFrame {
    let rate = mic.sample_rate_hz.max(system.sample_rate_hz);
    let mic_resampled = if mic.sample_rate_hz == rate {
        mic.clone()
    } else {
        resample_linear(mic, rate)
    };
    let sys_resampled = if system.sample_rate_hz == rate {
        system.clone()
    } else {
        resample_linear(system, rate)
    };

    let len = mic_resampled.samples.len().max(sys_resampled.samples.len());
    let mut samples = Vec::with_capacity(len);
    for i in 0..len {
        let m = mic_resampled.samples.get(i).copied().unwrap_or(0.0);
        let s = sys_resampled.samples.get(i).copied().unwrap_or(0.0);
        samples.push((m + s) / 2.0);
    }

    PcmFrame {
        samples,
        sample_rate_hz: rate,
    }
}

/// Downmix interleaved `channels`-channel audio to mono, converting each sample
/// with `to_f32` (identity for `f32` input, scaling for integer formats).
///
/// A trailing partial frame is dropped, as the capture callbacks only ever hand
/// us whole frames.
pub fn downmix_to_mono<S: Copy>(
    interleaved: &[S],
    channels: usize,
    to_f32: impl Fn(S) -> f32,
) -> Vec<f32> {
    if channels <= 1 {
        return interleaved.iter().map(|s| to_f32(*s)).collect();
    }
    let frames = interleaved.len() / channels;
    let mut mono = Vec::with_capacity(frames);
    for i in 0..frames {
        let base = i * channels;
        let sum: f32 = interleaved[base..base + channels]
            .iter()
            .map(|s| to_f32(*s))
            .sum();
        mono.push(sum / channels as f32);
    }
    mono
}

/// Split PCM into fixed-duration chunks for segmented STT (last chunk may be shorter).
pub fn chunk_pcm_by_duration(frame: &PcmFrame, chunk_secs: u32) -> Vec<PcmFrame> {
    if chunk_secs == 0 || frame.sample_rate_hz == 0 || frame.samples.is_empty() {
        return Vec::new();
    }

    let chunk_samples = (frame.sample_rate_hz as u64 * chunk_secs as u64) as usize;
    if chunk_samples == 0 {
        return Vec::new();
    }

    frame
        .samples
        .chunks(chunk_samples)
        .map(|chunk| PcmFrame {
            samples: chunk.to_vec(),
            sample_rate_hz: frame.sample_rate_hz,
        })
        .collect()
}

/// Where to cut a long buffer into chunks, avoiding mid-word boundaries.
///
/// `chunk_pcm_by_duration` splits on a hard sample count, which cuts through
/// whatever was being said. For a live meeting nobody sees that; for a
/// subtitle it is a word sliced in half across two cues. So near each target
/// boundary this searches +/-`search_secs` for the quietest 20 ms frame and
/// cuts there.
///
/// Deliberately no overlap between chunks. Overlapping windows mean the same
/// words are decoded twice and the seams have to be de-duplicated in text,
/// which is the approach that goes wrong — two decodes of the same audio
/// rarely produce the same string, so there is nothing reliable to match on.
///
/// How much quieter than the hard boundary a candidate frame must be before
/// the cut moves to it. Half the energy; see [`cut_points`] for why a bar
/// exists at all.
const QUIET_ENOUGH: f32 = 0.5;

/// Returns interior cut indices only: neither `0` nor `samples.len()`, so the
/// caller's chunk count is `cut_points(..).len() + 1`.
pub fn cut_points(samples: &[f32], rate_hz: u32, target_secs: u32, search_secs: u32) -> Vec<usize> {
    if rate_hz == 0 || target_secs == 0 || samples.is_empty() {
        return Vec::new();
    }
    let target = rate_hz as usize * target_secs as usize;
    if target == 0 || samples.len() <= target {
        return Vec::new();
    }
    let search = rate_hz as usize * search_secs as usize;
    // 20 ms, the shortest window whose RMS still means "quiet" rather than
    // "happened to land between two glottal pulses".
    let frame = (rate_hz as usize / 50).max(1);

    let mut cuts = Vec::new();
    let mut boundary = target;
    while boundary < samples.len() {
        let lo = boundary
            .saturating_sub(search)
            .max(cuts.last().copied().unwrap_or(0) + frame);
        let hi = (boundary + search).min(samples.len().saturating_sub(frame));
        let mut best = boundary.min(samples.len().saturating_sub(frame));
        if lo < hi && best + frame <= samples.len() {
            // The bar a candidate has to clear, not `f32::MAX`. Taking the
            // window minimum outright is what the naive version does, and on
            // continuous speech — where no frame is actually quiet — it picks
            // whichever frame happened to be marginally softest, usually near
            // the start of the search. Each cut then reseeds the next
            // boundary from there, so the chunks march steadily shorter: a
            // 30 s target produced 1 s chunks. A candidate must be
            // meaningfully quieter than the hard boundary to move the cut at
            // all; otherwise the boundary stands.
            let mut best_rms = rms_level(&samples[best..best + frame]) * QUIET_ENOUGH;
            let mut at = lo;
            while at < hi {
                let level = rms_level(&samples[at..at + frame]);
                if level < best_rms {
                    best_rms = level;
                    best = at;
                }
                at += frame;
            }
        }
        // Cut in the middle of the quiet frame, not at its leading edge:
        // that keeps the trailing consonant with the chunk it belongs to.
        let cut = (best + frame / 2).min(samples.len());
        if cut <= cuts.last().copied().unwrap_or(0) || cut >= samples.len() {
            break;
        }
        cuts.push(cut);
        boundary = cut + target;
    }
    cuts
}

/// Concatenate frames assumed to share the same sample rate.
pub fn accumulate_frames(frames: &[PcmFrame]) -> PcmFrame {
    if frames.is_empty() {
        return PcmFrame {
            samples: Vec::new(),
            sample_rate_hz: 0,
        };
    }

    let sample_rate_hz = frames[0].sample_rate_hz;
    let total: usize = frames.iter().map(|f| f.samples.len()).sum();
    let mut samples = Vec::with_capacity(total);
    for frame in frames {
        samples.extend_from_slice(&frame.samples);
    }

    PcmFrame {
        samples,
        sample_rate_hz,
    }
}

/// A fixed-size ring of the most recent mono samples.
///
/// Backs the dictation preroll: capture is armed on the first modifier of the
/// ⌥⇧ chord and writes here until the hold threshold passes, at which point the
/// ring is drained ahead of the live audio. Everything older than the window
/// simply falls out the back, so an armed-but-never-used chord costs a fixed
/// amount of memory no matter how long the user rests on the key.
///
/// Deliberately not a `VecDeque`: the write path runs inside the cpal callback
/// and must not allocate, and a pre-sized `Vec` with a write cursor never does.
#[derive(Debug)]
pub struct RingBuffer {
    buf: Vec<f32>,
    /// Where the next sample goes.
    write: usize,
    /// How many of `buf`'s slots hold real audio; saturates at capacity.
    filled: usize,
}

impl RingBuffer {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: vec![0.0; capacity],
            write: 0,
            filled: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn len(&self) -> usize {
        self.filled
    }

    pub fn is_empty(&self) -> bool {
        self.filled == 0
    }

    /// Append `samples`, dropping whatever no longer fits in the window.
    pub fn push(&mut self, samples: &[f32]) {
        let capacity = self.buf.len();
        if capacity == 0 {
            return;
        }
        // A buffer larger than the whole window would wrap over itself; only
        // its tail could survive, so copy just that.
        let tail = if samples.len() > capacity {
            &samples[samples.len() - capacity..]
        } else {
            samples
        };
        for sample in tail {
            self.buf[self.write] = *sample;
            self.write = (self.write + 1) % capacity;
        }
        self.filled = (self.filled + tail.len()).min(capacity);
    }

    /// Take the window in recording order, oldest first, leaving the ring empty.
    pub fn drain(&mut self) -> Vec<f32> {
        let capacity = self.buf.len();
        let mut out = Vec::with_capacity(self.filled);
        if self.filled > 0 {
            // Once full, the oldest sample is the one the cursor is about to
            // overwrite; before that, the ring has never wrapped and the oldest
            // sample is at zero.
            let start = if self.filled == capacity {
                self.write
            } else {
                0
            };
            for i in 0..self.filled {
                out.push(self.buf[(start + i) % capacity]);
            }
        }
        self.clear();
        out
    }

    pub fn clear(&mut self) {
        self.write = 0;
        self.filled = 0;
    }
}

/// Which input device capture should open, and whether the saved preference
/// was honoured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceChoice {
    /// Nothing saved: whatever the OS calls the default input.
    Default,
    /// The saved device is present.
    Preferred(String),
    /// The saved device is gone — unplugged, undocked, or a Bluetooth headset
    /// that wandered off. Capture falls back to the default rather than
    /// failing, and `fallback` is what the UI says about it.
    Fallback(DeviceFallback),
}

/// Decide which input device to open, given the current enumeration and the
/// saved preference.
///
/// Pure so the three cases are testable without a `cpal` host. Device names are
/// the only handle `cpal` gives, and they are neither unique nor stable across
/// reboots on every host, so this resolves leniently and never errors: a
/// preference that no longer matches anything is a fallback, not a failure.
pub fn choose_input_device(devices: &[InputDevice], preferred: Option<&str>) -> DeviceChoice {
    let Some(preferred) = preferred else {
        return DeviceChoice::Default;
    };
    if devices.iter().any(|d| d.id == preferred) {
        return DeviceChoice::Preferred(preferred.to_string());
    }
    DeviceChoice::Fallback(DeviceFallback {
        requested: preferred.to_string(),
        using: devices
            .iter()
            .find(|d| d.is_default)
            .map(|d| d.name.clone()),
    })
}

#[cfg(test)]
mod tests {

    /// The point of the search: land in the silence, not on the arithmetic
    /// boundary that falls mid-word.
    #[test]
    fn cut_points_prefer_the_quiet_gap_near_the_boundary() {
        let rate = 16_000u32;
        // 6 s of tone with a 200 ms silence starting 0.5 s *after* the 3 s
        // boundary, well inside the +/-2 s search window.
        let mut samples = vec![0.0f32; rate as usize * 6];
        for (i, s) in samples.iter_mut().enumerate() {
            let t = i as f32 / rate as f32;
            *s = (std::f32::consts::TAU * 200.0 * t).sin() * 0.5;
        }
        let gap_start = (rate as f32 * 3.5) as usize;
        let gap_end = gap_start + (rate as usize) / 5;
        for s in &mut samples[gap_start..gap_end] {
            *s = 0.0;
        }

        let cuts = cut_points(&samples, rate, 3, 2);
        assert_eq!(cuts.len(), 1, "{cuts:?}");
        assert!(
            cuts[0] >= gap_start && cuts[0] <= gap_end,
            "cut at {} is outside the silence {gap_start}..{gap_end}",
            cuts[0]
        );
    }

    /// With no quiet anywhere, the hard boundary is still the answer — a
    /// chunker that refused to cut a loud file would never chunk a podcast.
    #[test]
    fn a_uniformly_loud_buffer_still_cuts_near_the_target() {
        let rate = 16_000u32;
        let samples: Vec<f32> = (0..rate as usize * 6)
            .map(|i| ((i % 37) as f32 / 37.0) - 0.5)
            .collect();
        let cuts = cut_points(&samples, rate, 3, 2);
        assert_eq!(cuts.len(), 1);
        let boundary = rate as i64 * 3;
        assert!(
            (cuts[0] as i64 - boundary).abs() <= rate as i64 * 2,
            "cut {} strayed outside the search window around {boundary}",
            cuts[0]
        );
    }

    #[test]
    fn a_buffer_shorter_than_one_chunk_is_never_cut() {
        let samples = vec![0.1f32; 16_000];
        assert!(cut_points(&samples, 16_000, 30, 2).is_empty());
        assert!(cut_points(&[], 16_000, 30, 2).is_empty());
        assert!(cut_points(&samples, 0, 30, 2).is_empty());
        assert!(cut_points(&samples, 16_000, 0, 2).is_empty());
    }

    /// Cuts are interior and strictly increasing: anything else makes the
    /// chunk driver produce an empty or a backwards chunk.
    #[test]
    fn cuts_are_interior_and_monotonic() {
        let rate = 16_000u32;
        let samples: Vec<f32> = (0..rate as usize * 95)
            .map(|i| ((i % 101) as f32 / 101.0) - 0.5)
            .collect();
        let cuts = cut_points(&samples, rate, 30, 2);
        assert!(cuts.len() >= 2, "{} cuts over 95s", cuts.len());
        assert!(cuts.iter().all(|&c| c > 0 && c < samples.len()));
        assert!(cuts.windows(2).all(|w| w[0] < w[1]));
    }
    use super::*;

    fn frame(n: usize) -> PcmFrame {
        PcmFrame {
            samples: vec![0.1; n],
            sample_rate_hz: 16_000,
        }
    }

    fn device(name: &str, is_default: bool) -> InputDevice {
        InputDevice {
            id: name.to_string(),
            name: name.to_string(),
            is_default,
        }
    }

    #[test]
    fn no_preference_takes_the_default_device() {
        let devices = [
            device("MacBook Air Microphone", true),
            device("Yeti", false),
        ];
        assert_eq!(choose_input_device(&devices, None), DeviceChoice::Default);
    }

    #[test]
    fn a_present_preference_is_honoured() {
        let devices = [
            device("MacBook Air Microphone", true),
            device("Yeti", false),
        ];
        assert_eq!(
            choose_input_device(&devices, Some("Yeti")),
            DeviceChoice::Preferred("Yeti".into())
        );
    }

    #[test]
    fn an_absent_preference_falls_back_and_names_both_devices() {
        // The docking-station case: the Yeti the user picked is gone, and the
        // UI has to be able to say which mic is recording instead.
        let devices = [device("MacBook Air Microphone", true)];
        assert_eq!(
            choose_input_device(&devices, Some("Yeti")),
            DeviceChoice::Fallback(DeviceFallback {
                requested: "Yeti".into(),
                using: Some("MacBook Air Microphone".into()),
            })
        );
    }

    #[test]
    fn a_fallback_with_no_devices_at_all_still_resolves() {
        // Every input vanished. Resolution must still answer; opening the
        // stream is what fails, with the error the caller already handles.
        assert_eq!(
            choose_input_device(&[], Some("Yeti")),
            DeviceChoice::Fallback(DeviceFallback {
                requested: "Yeti".into(),
                using: None,
            })
        );
    }

    #[test]
    fn a_partially_filled_ring_drains_in_recording_order() {
        let mut ring = RingBuffer::with_capacity(8);
        ring.push(&[1.0, 2.0, 3.0]);
        assert_eq!(ring.len(), 3);
        assert_eq!(ring.drain(), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn a_wrapped_ring_keeps_only_the_most_recent_window() {
        let mut ring = RingBuffer::with_capacity(4);
        ring.push(&[1.0, 2.0, 3.0]);
        ring.push(&[4.0, 5.0]);
        assert_eq!(ring.len(), 4);
        assert_eq!(
            ring.drain(),
            vec![2.0, 3.0, 4.0, 5.0],
            "the oldest sample falls out the back, in order"
        );
    }

    #[test]
    fn a_single_buffer_larger_than_the_window_keeps_its_tail() {
        // A device handing us a 1024-sample buffer into a shorter ring: only
        // the newest samples can survive, and they must stay in order.
        let mut ring = RingBuffer::with_capacity(3);
        ring.push(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        assert_eq!(ring.drain(), vec![3.0, 4.0, 5.0]);
    }

    #[test]
    fn draining_clears_the_ring() {
        let mut ring = RingBuffer::with_capacity(4);
        ring.push(&[1.0, 2.0]);
        assert_eq!(ring.drain(), vec![1.0, 2.0]);
        assert!(ring.is_empty());
        assert!(
            ring.drain().is_empty(),
            "a drained preroll must not be replayed into the next recording"
        );
    }

    #[test]
    fn a_zero_capacity_ring_swallows_everything() {
        // What a device that reports a zero sample rate would produce. It must
        // not panic in the audio callback.
        let mut ring = RingBuffer::with_capacity(0);
        ring.push(&[1.0, 2.0]);
        assert!(ring.is_empty());
        assert!(ring.drain().is_empty());
    }

    #[test]
    fn send_to_a_live_consumer_counts_nothing() {
        let (tx, _rx) = tokio::sync::mpsc::channel(4);
        let counters = FrameCounters::new();

        assert!(counters.send(&tx, frame(8), "dictation"));
        assert_eq!(counters.totals(), (0, 0));
    }

    /// The bug this type exists for: with no receiver, `try_send` fails with
    /// `Closed`, which the old code counted and reported as a full channel.
    /// Nothing is lost in this case — the caller still buffers the frame — so it
    /// must not land in the `full` bucket that drives the warning.
    #[test]
    fn send_with_no_consumer_counts_closed_not_full() {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        drop(rx);
        let counters = FrameCounters::new();

        for _ in 0..250 {
            assert!(!counters.send(&tx, frame(8), "dictation"));
        }

        let (full, closed) = counters.totals();
        assert_eq!(full, 0, "a closed channel is not a full one");
        assert_eq!(closed, 250);
    }

    #[test]
    fn send_to_a_backed_up_consumer_counts_full() {
        let (tx, _rx) = tokio::sync::mpsc::channel(2);
        let counters = FrameCounters::new();

        assert!(counters.send(&tx, frame(8), "meeting"));
        assert!(counters.send(&tx, frame(8), "meeting"));
        // Capacity is spent and the receiver is alive but never reading.
        assert!(!counters.send(&tx, frame(8), "meeting"));

        assert_eq!(counters.totals(), (1, 0));
    }

    #[test]
    fn reset_clears_both_counters() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        let counters = FrameCounters::new();
        counters.send(&tx, frame(4), "dictation");
        assert_eq!(counters.totals().1, 1);

        counters.reset();
        assert_eq!(counters.totals(), (0, 0));
    }

    #[test]
    fn log_session_drains_the_counters() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        let counters = FrameCounters::new();
        counters.send(&tx, frame(4), "dictation");

        counters.log_session("dictation");
        assert_eq!(
            counters.totals(),
            (0, 0),
            "a session summary consumes what it reported"
        );
    }

    #[test]
    fn counters_are_shared_across_clones() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        let counters = FrameCounters::new();
        // The capture thread gets a clone; `stop_mic` reads the original.
        let on_capture_thread = counters.clone();
        on_capture_thread.send(&tx, frame(4), "dictation");

        assert_eq!(counters.totals(), (0, 1));
    }

    #[test]
    fn resample_halves_sample_count_when_halving_rate() {
        let frame = PcmFrame {
            samples: (0..100).map(|i| i as f32 / 100.0).collect(),
            sample_rate_hz: 48_000,
        };
        let out = resample_linear(&frame, 24_000);
        assert_eq!(out.sample_rate_hz, 24_000);
        assert_eq!(out.samples.len(), 50);
    }

    #[test]
    fn rms_silence_is_zero() {
        assert_eq!(rms_level(&[0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn rms_full_scale_is_one() {
        assert!((rms_level(&[1.0, -1.0, 1.0]) - 1.0).abs() < 0.01);
    }

    #[test]
    fn mix_frames_averages_aligned_samples() {
        let mic = PcmFrame {
            samples: vec![1.0, 0.0],
            sample_rate_hz: 16_000,
        };
        let sys = PcmFrame {
            samples: vec![0.0, 1.0],
            sample_rate_hz: 16_000,
        };
        let mixed = mix_frames(&mic, &sys);
        assert_eq!(mixed.samples.len(), 2);
        assert!((mixed.samples[0] - 0.5).abs() < 0.01);
        assert!((mixed.samples[1] - 0.5).abs() < 0.01);
    }

    #[test]
    fn chunk_90s_audio_into_three_30s_segments() {
        let frame = PcmFrame {
            samples: vec![0.0; 16_000 * 90],
            sample_rate_hz: 16_000,
        };
        let chunks = chunk_pcm_by_duration(&frame, 30);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].samples.len(), 16_000 * 30);
        assert_eq!(chunks[2].samples.len(), 16_000 * 30);
    }

    #[test]
    fn downmix_passes_mono_through_the_converter() {
        let mono = downmix_to_mono(&[0i16, i16::MAX], 1, |s| s as f32 / i16::MAX as f32);
        assert_eq!(mono.len(), 2);
        assert!((mono[1] - 1.0).abs() < 0.001);
    }

    #[test]
    fn downmix_averages_interleaved_channels() {
        let mono = downmix_to_mono(&[1.0f32, 0.0, 0.0, 1.0], 2, |s| s);
        assert_eq!(mono, vec![0.5, 0.5]);
    }

    #[test]
    fn downmix_drops_trailing_partial_frame() {
        let mono = downmix_to_mono(&[1.0f32, 1.0, 1.0], 2, |s| s);
        assert_eq!(mono, vec![1.0]);
    }

    #[test]
    fn accumulate_frames_concatenates_samples() {
        let frames = vec![
            PcmFrame {
                samples: vec![0.1, 0.2],
                sample_rate_hz: 16_000,
            },
            PcmFrame {
                samples: vec![0.3],
                sample_rate_hz: 16_000,
            },
        ];
        let out = accumulate_frames(&frames);
        assert_eq!(out.samples, vec![0.1, 0.2, 0.3]);
        assert_eq!(out.sample_rate_hz, 16_000);
    }
}
