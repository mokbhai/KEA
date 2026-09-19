//! Pure audio helpers (resample, RMS, frame accumulation) plus the frame-sink
//! bookkeeping shared by the capture callbacks.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::mpsc::Sender;

use super::PcmFrame;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: usize) -> PcmFrame {
        PcmFrame {
            samples: vec![0.1; n],
            sample_rate_hz: 16_000,
        }
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
