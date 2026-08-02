//! Synthesized UI cue tones for dictation feedback.
//!
//! Nothing is bundled: each cue is generated as mono PCM on demand and handed
//! to [`super::playback::play_pcm_blocking`]. Cues stay short (<300 ms) and
//! quiet so they read as feedback rather than as a notification.
//!
//! # Manual verification (macOS)
//! 1. Run KEA, dictate a phrase — a two-note rising chime after the insert.
//! 2. Dictate with no STT engine bound — a two-note falling buzz.
//! 3. Dictate silence — a single neutral blip.
//! 4. Unit tests cover the generator only; they assert no audible output.

use std::f32::consts::TAU;

use super::PcmFrame;

/// Cue rendering rate. Any device rate works — rodio resamples — and 44.1 kHz
/// is high enough that the highest cue partial is nowhere near Nyquist.
pub const CUE_SAMPLE_RATE_HZ: u32 = 44_100;

/// Peak amplitude of every cue. Feedback, not an alert.
pub const CUE_AMPLITUDE: f32 = 0.2;

/// Fade applied to each end of a tone. Long enough to kill the click of a
/// waveform that starts or stops mid-cycle, short enough not to swallow a
/// 60 ms note.
const FADE_MS: u32 = 8;

/// The three outcomes dictation can announce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// A run finished and text was inserted.
    Success,
    /// A run failed.
    Error,
    /// A run ended without producing anything.
    Cancel,
}

/// One note of a cue.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Tone {
    freq_hz: f32,
    duration_ms: u32,
}

impl Cue {
    /// The notes this cue is built from, in order.
    fn tones(self) -> &'static [Tone] {
        match self {
            // Rising major sixth: the "done" shape most desktop apps use.
            Cue::Success => &[
                Tone { freq_hz: 784.0, duration_ms: 55 },
                Tone { freq_hz: 1318.5, duration_ms: 75 },
            ],
            // Falling, and lower throughout, so it is distinguishable from
            // success without relying on the listener catching both notes.
            Cue::Error => &[
                Tone { freq_hz: 415.3, duration_ms: 90 },
                Tone { freq_hz: 277.2, duration_ms: 120 },
            ],
            // Single neutral blip: neither rising nor falling.
            Cue::Cancel => &[Tone { freq_hz: 587.3, duration_ms: 90 }],
        }
    }
}

/// Number of samples a duration occupies at a rate, rounded to nearest.
pub fn samples_for_duration(duration_ms: u32, sample_rate_hz: u32) -> usize {
    ((u64::from(duration_ms) * u64::from(sample_rate_hz) + 500) / 1000) as usize
}

/// Applies a linear fade-in and fade-out in place.
///
/// The fade is capped at half the buffer so the two ramps never overlap and
/// re-amplify each other on a note shorter than `2 * fade_len`.
pub fn apply_fade_envelope(samples: &mut [f32], fade_len: usize) {
    let len = samples.len();
    if len == 0 {
        return;
    }
    let fade = fade_len.min(len / 2);
    if fade == 0 {
        // Too short for a ramp: silence the endpoints so playback still
        // starts and ends at zero.
        samples[0] = 0.0;
        samples[len - 1] = 0.0;
        return;
    }
    for i in 0..fade {
        let gain = i as f32 / fade as f32;
        samples[i] *= gain;
        samples[len - 1 - i] *= gain;
    }
}

/// Renders a single faded sine tone.
fn render_tone(tone: Tone, sample_rate_hz: u32, amplitude: f32) -> Vec<f32> {
    let n = samples_for_duration(tone.duration_ms, sample_rate_hz);
    let mut samples: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f32 / sample_rate_hz as f32;
            amplitude * (TAU * tone.freq_hz * t).sin()
        })
        .collect();
    apply_fade_envelope(
        &mut samples,
        samples_for_duration(FADE_MS, sample_rate_hz),
    );
    samples
}

/// Renders `cue` at [`CUE_SAMPLE_RATE_HZ`].
pub fn cue_pcm(cue: Cue) -> PcmFrame {
    cue_pcm_at(cue, CUE_SAMPLE_RATE_HZ, CUE_AMPLITUDE)
}

/// Renders `cue` at an explicit rate and peak amplitude.
pub fn cue_pcm_at(cue: Cue, sample_rate_hz: u32, amplitude: f32) -> PcmFrame {
    let samples = cue
        .tones()
        .iter()
        .flat_map(|tone| render_tone(*tone, sample_rate_hz, amplitude))
        .collect();
    PcmFrame {
        samples,
        sample_rate_hz,
    }
}

/// Total length of a cue in milliseconds.
pub fn cue_duration_ms(cue: Cue) -> u32 {
    cue.tones().iter().map(|t| t.duration_ms).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Cue; 3] = [Cue::Success, Cue::Error, Cue::Cancel];

    #[test]
    fn samples_for_duration_rounds_to_nearest() {
        assert_eq!(samples_for_duration(1000, 48_000), 48_000);
        assert_eq!(samples_for_duration(100, 44_100), 4410);
        assert_eq!(samples_for_duration(0, 44_100), 0);
    }

    #[test]
    fn cue_pcm_has_the_requested_sample_count_and_rate() {
        for cue in ALL {
            let frame = cue_pcm_at(cue, 16_000, CUE_AMPLITUDE);
            let expected: usize = cue
                .tones()
                .iter()
                .map(|t| samples_for_duration(t.duration_ms, 16_000))
                .sum();
            assert_eq!(frame.samples.len(), expected, "{cue:?}");
            assert_eq!(frame.sample_rate_hz, 16_000);
        }
    }

    #[test]
    fn cue_pcm_stays_within_the_requested_amplitude() {
        for cue in ALL {
            let frame = cue_pcm(cue);
            let peak = frame.samples.iter().fold(0.0_f32, |m, s| m.max(s.abs()));
            assert!(peak <= CUE_AMPLITUDE + 1e-6, "{cue:?} peaked at {peak}");
            // A cue that generated nothing audible would also pass the ceiling.
            assert!(peak > CUE_AMPLITUDE * 0.5, "{cue:?} is inaudibly quiet");
        }
    }

    #[test]
    fn cue_pcm_starts_and_ends_near_zero() {
        for cue in ALL {
            let frame = cue_pcm(cue);
            let first = frame.samples.first().copied().unwrap();
            let last = frame.samples.last().copied().unwrap();
            assert!(first.abs() < 1e-3, "{cue:?} starts at {first}");
            assert!(last.abs() < 1e-3, "{cue:?} ends at {last}");
        }
    }

    #[test]
    fn every_cue_differs_from_every_other() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(cue_pcm(*a).samples, cue_pcm(*b).samples, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn every_cue_is_short() {
        for cue in ALL {
            assert!(cue_duration_ms(cue) < 300, "{cue:?} is too long");
        }
    }

    #[test]
    fn apply_fade_envelope_ramps_both_ends() {
        let mut samples = vec![1.0_f32; 10];
        apply_fade_envelope(&mut samples, 4);
        assert_eq!(samples[0], 0.0);
        assert_eq!(samples[9], 0.0);
        assert!(samples[1] < samples[2]);
        assert!(samples[8] < samples[7]);
        assert_eq!(samples[5], 1.0);
    }

    #[test]
    fn apply_fade_envelope_never_overlaps_its_ramps() {
        // A fade longer than the buffer would otherwise scale some samples
        // twice, or amplify them past the peak.
        let mut samples = vec![1.0_f32; 5];
        apply_fade_envelope(&mut samples, 100);
        assert!(samples.iter().all(|s| (0.0..=1.0).contains(s)));
        assert_eq!(samples[0], 0.0);
        assert_eq!(samples[4], 0.0);
    }

    #[test]
    fn apply_fade_envelope_handles_degenerate_buffers() {
        apply_fade_envelope(&mut [], 4);
        let mut one = vec![1.0_f32];
        apply_fade_envelope(&mut one, 4);
        assert_eq!(one, vec![0.0]);
    }

    #[test]
    fn cue_pcm_is_playable() {
        for cue in ALL {
            assert!(super::super::playback::validate_pcm_for_playback(&cue_pcm(cue)).is_ok());
        }
    }
}
