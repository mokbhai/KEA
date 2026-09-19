//! "You" vs "Others" from which device the audio arrived on.
//!
//! ## Why this is the better default, and the model is not
//!
//! For the dominant meeting shape — one local person on a video call — two
//! labels is the entire useful answer, and here they are grounded in physics
//! (the mic heard it, or the loopback did) rather than in a clustering
//! threshold. It costs no download, no inference and no GPU, it cannot
//! confuse two people with similar voices, and it degrades honestly: when the
//! channels are ambiguous it says [`SpeakerChannel::Mixed`] instead of
//! guessing, which a clusterer never does.
//!
//! What it cannot do is tell two remote participants apart, and in
//! `CaptureMode::MicOnly` every segment is `Local`, which is correct and
//! useless. That is the case a model exists for — see
//! `kea_infer::sherpa_diarize`.
//!
//! Pure over two slices, so the whole decision is testable with synthesized
//! buffers and no audio device. That is the only seam in this feature that
//! can be tested cheaply, which is why the decision lives here in core rather
//! than in the capture callback.

use serde::{Deserialize, Serialize};

/// Which side of a two-channel meeting a segment came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerChannel {
    /// The microphone: the person sitting here.
    Local,
    /// System/loopback audio: whoever is on the call.
    Remote,
    /// Both were hot, or neither was. Not a failure — on speakers rather than
    /// headphones the mic hears the far side, and saying so is more useful
    /// than picking the louder one by a hair.
    Mixed,
}

impl SpeakerChannel {
    /// The persisted spelling, written once. See `MeetingStatus` for the
    /// pattern and why it is not a bare string.
    pub fn as_str(&self) -> &'static str {
        match self {
            SpeakerChannel::Local => "local",
            SpeakerChannel::Remote => "remote",
            SpeakerChannel::Mixed => "mixed",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "local" => Some(SpeakerChannel::Local),
            "remote" => Some(SpeakerChannel::Remote),
            "mixed" => Some(SpeakerChannel::Mixed),
            _ => None,
        }
    }

    /// The name shown in a transcript before anyone renames it.
    pub fn display_name(&self) -> &'static str {
        match self {
            SpeakerChannel::Local => "You",
            SpeakerChannel::Remote => "Others",
            SpeakerChannel::Mixed => "Speaker",
        }
    }
}

/// Window length the decision is made over.
///
/// 200 ms: long enough that one glottal closure cannot swing it, short enough
/// that a two-word interjection still gets a window of its own.
const WINDOW_MS: usize = 200;

/// How much louder one channel must be to take a window, in dB.
///
/// Six, which is a factor of two in amplitude. Below that, acoustic echo —
/// the mic hearing the far side through the speakers — routinely wins
/// windows for the wrong channel.
const MARGIN_DB: f32 = 6.0;

/// RMS below which a window is silence and takes no part in the vote.
///
/// Without a floor, the gaps between words — where both channels are near
/// zero and the margin is decided by dither — outnumber the speech and
/// decide the segment.
const SPEECH_FLOOR: f32 = 0.005;

/// The fraction of speech-active windows one channel must win to take the
/// segment outright.
const MAJORITY: f32 = 0.6;

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Decides which channel a segment belongs to.
///
/// `mic` and `system` are the two per-source buffers for the *same* span, at
/// the same rate, and must be sample-aligned — cutting them at the same index
/// is what the capture path owes this function.
pub fn attribute_segment(mic: &[f32], system: &[f32], rate_hz: u32) -> SpeakerChannel {
    if rate_hz == 0 {
        return SpeakerChannel::Mixed;
    }
    let window = (rate_hz as usize * WINDOW_MS / 1000).max(1);
    let frames = mic.len().max(system.len()).div_ceil(window);

    // A factor, not a dB comparison at each window: `10^(6/20)`, computed
    // once, and it sidesteps `log10(0)` on a silent channel entirely.
    let margin = 10f32.powf(MARGIN_DB / 20.0);

    let mut mic_wins = 0usize;
    let mut system_wins = 0usize;
    let mut active = 0usize;

    for i in 0..frames {
        let start = i * window;
        let slice = |buf: &[f32]| -> f32 {
            if start >= buf.len() {
                return 0.0;
            }
            rms(&buf[start..(start + window).min(buf.len())])
        };
        let mic_level = slice(mic);
        let system_level = slice(system);
        if mic_level < SPEECH_FLOOR && system_level < SPEECH_FLOOR {
            continue;
        }
        active += 1;
        if mic_level > system_level * margin {
            mic_wins += 1;
        } else if system_level > mic_level * margin {
            system_wins += 1;
        }
    }

    if active == 0 {
        return SpeakerChannel::Mixed;
    }
    let needed = (active as f32 * MAJORITY).ceil() as usize;
    if mic_wins >= needed {
        SpeakerChannel::Local
    } else if system_wins >= needed {
        SpeakerChannel::Remote
    } else {
        SpeakerChannel::Mixed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn tone(secs: f32, amplitude: f32) -> Vec<f32> {
        let n = (RATE as f32 * secs) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                (std::f32::consts::TAU * 220.0 * t).sin() * amplitude
            })
            .collect()
    }

    #[test]
    fn a_mic_dominant_segment_is_local() {
        let mic = tone(2.0, 0.5);
        let system = tone(2.0, 0.01);
        assert_eq!(
            attribute_segment(&mic, &system, RATE),
            SpeakerChannel::Local
        );
    }

    #[test]
    fn a_system_dominant_segment_is_remote() {
        let mic = tone(2.0, 0.01);
        let system = tone(2.0, 0.5);
        assert_eq!(
            attribute_segment(&mic, &system, RATE),
            SpeakerChannel::Remote
        );
    }

    /// Both hot is the acoustic-echo case — speakers rather than headphones —
    /// and declining to guess is the honest answer.
    #[test]
    fn both_channels_loud_is_mixed() {
        let mic = tone(2.0, 0.4);
        let system = tone(2.0, 0.4);
        assert_eq!(
            attribute_segment(&mic, &system, RATE),
            SpeakerChannel::Mixed
        );
    }

    /// Silence has no speaker. Without the floor, the near-zero windows
    /// between words decide the segment on dither.
    #[test]
    fn both_channels_silent_is_mixed() {
        let silence = vec![0.0f32; RATE as usize];
        assert_eq!(
            attribute_segment(&silence, &silence, RATE),
            SpeakerChannel::Mixed
        );
        assert_eq!(attribute_segment(&[], &[], RATE), SpeakerChannel::Mixed);
    }

    /// The boundary. Asserted either side of the margin rather than exactly
    /// on it: two sine buffers scaled by the margin differ in RMS only by
    /// float rounding, so a test pinned to the exact tie would be asserting
    /// the mood of the FPU rather than the rule.
    #[test]
    fn the_margin_is_what_decides_a_close_segment() {
        let margin = 10f32.powf(MARGIN_DB / 20.0);
        let system = tone(2.0, 0.1);

        let just_under = tone(2.0, 0.1 * margin * 0.95);
        assert_eq!(
            attribute_segment(&just_under, &system, RATE),
            SpeakerChannel::Mixed,
            "a lead under the margin is not a lead"
        );

        let just_over = tone(2.0, 0.1 * margin * 1.05);
        assert_eq!(
            attribute_segment(&just_over, &system, RATE),
            SpeakerChannel::Local
        );
    }

    /// A segment where each side talks half the time belongs to neither.
    #[test]
    fn alternating_speakers_are_mixed() {
        let mut mic = tone(2.0, 0.5);
        let mut system = tone(2.0, 0.5);
        let half = mic.len() / 2;
        for s in &mut mic[half..] {
            *s = 0.0;
        }
        for s in &mut system[..half] {
            *s = 0.0;
        }
        assert_eq!(
            attribute_segment(&mic, &system, RATE),
            SpeakerChannel::Mixed
        );
    }

    /// Unequal lengths must not panic or read past the shorter buffer: the
    /// drain cuts both channels at one index but a trailing frame can differ.
    #[test]
    fn unequal_buffer_lengths_are_tolerated() {
        let mic = tone(2.0, 0.5);
        let system = tone(1.0, 0.01);
        assert_eq!(
            attribute_segment(&mic, &system, RATE),
            SpeakerChannel::Local
        );
        assert_eq!(
            attribute_segment(&system, &mic, RATE),
            SpeakerChannel::Remote
        );
    }

    #[test]
    fn channel_names_keep_their_stored_spelling() {
        for (channel, text) in [
            (SpeakerChannel::Local, "local"),
            (SpeakerChannel::Remote, "remote"),
            (SpeakerChannel::Mixed, "mixed"),
        ] {
            assert_eq!(channel.as_str(), text);
            assert_eq!(SpeakerChannel::from_str(text), Some(channel));
            assert_eq!(
                serde_json::to_string(&channel).unwrap(),
                format!("\"{text}\"")
            );
        }
        assert_eq!(SpeakerChannel::Local.display_name(), "You");
        assert_eq!(SpeakerChannel::from_str("nobody"), None);
    }

    #[test]
    fn a_zero_sample_rate_is_refused_rather_than_dividing_by_zero() {
        assert_eq!(
            attribute_segment(&tone(1.0, 0.5), &[], 0),
            SpeakerChannel::Mixed
        );
    }
}
