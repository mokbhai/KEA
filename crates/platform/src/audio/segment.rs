//! Deciding *where* to end a meeting segment.
//!
//! Cutting on a wall clock splits words: the boundary lands wherever the timer
//! fires, so a word straddling it is transcribed wrong on both sides and the
//! model loses the sentence either way. Instead we cut where the speaker
//! already paused, and only fall back to the clock when they never do.
//!
//! Speech is detected by short-term energy against an adaptive noise floor
//! rather than a fixed threshold, because the floor differs wildly between a
//! quiet room and a noisy call. Answering "has anyone spoken in the last few
//! seconds" is far more forgiving than frame-level voice detection, so this
//! stays a simple energy test; a neural VAD can replace [`speech_mask`] later
//! without touching the cut rules.

/// Window used for the energy envelope. Long enough to smooth over the gaps
/// inside normal speech, short enough to locate a pause precisely.
const FRAME_MS: usize = 20;

/// Speech must exceed the noise floor by this factor. Conversational speech
/// runs an order of magnitude above room tone; 3x keeps breath and keyboard
/// noise out without clipping soft talkers.
const SPEECH_OVER_NOISE: f32 = 3.0;

/// Absolute floor so a silent room (noise ≈ 0) cannot make any faint sound
/// register as speech.
const MIN_SPEECH_RMS: f32 = 0.006;

/// Padding kept either side of speech so a cut never clips the final consonant.
const PAD_MS: usize = 250;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentCutConfig {
    /// Trailing silence that ends a segment early, in seconds.
    pub pause_secs: f32,
    /// Hard cap — cut here even mid-sentence. The user's segment length.
    pub max_secs: f32,
    /// Ignore pauses before this much audio, so one word plus a pause does not
    /// become its own tiny segment (short clips transcribe poorly).
    pub min_secs: f32,
}

impl Default for SegmentCutConfig {
    fn default() -> Self {
        Self {
            pause_secs: 3.0,
            max_secs: 30.0,
            min_secs: 5.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentCut {
    /// How many samples to take for this segment.
    pub take: usize,
    /// Whether any speech was found in them. A segment without speech is not
    /// worth transcribing — Whisper invents text when given silence.
    pub has_speech: bool,
}

fn frame_len(sample_rate_hz: u32) -> usize {
    ((sample_rate_hz as usize * FRAME_MS) / 1000).max(1)
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    (sum_sq / samples.len() as f32).sqrt()
}

/// Per-frame speech/silence decision using a noise floor taken from the
/// quietest part of this buffer.
fn speech_mask(samples: &[f32], sample_rate_hz: u32) -> Vec<bool> {
    let flen = frame_len(sample_rate_hz);
    let levels: Vec<f32> = samples.chunks(flen).map(rms).collect();
    if levels.is_empty() {
        return Vec::new();
    }
    let mut sorted = levels.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // 10th percentile approximates room tone without being dragged down by a
    // single dead frame; 90th stands in for how loud this speaker is.
    let noise = sorted[sorted.len() / 10];
    let loud = sorted[sorted.len() * 9 / 10];
    // The floor must stay well under the loud level. Without that ceiling a
    // buffer of unbroken speech calibrates its own noise floor to speech and
    // detects none of it — the whole segment then looks silent.
    let threshold = (noise * SPEECH_OVER_NOISE)
        .min(loud * 0.35)
        .max(MIN_SPEECH_RMS);
    levels.iter().map(|l| *l > threshold).collect()
}

/// Decides whether `samples` can be cut yet, and where.
///
/// Returns `None` while the buffer is still short of both a qualifying pause
/// and the hard cap — the caller keeps accumulating.
pub fn find_segment_cut(
    samples: &[f32],
    sample_rate_hz: u32,
    cfg: SegmentCutConfig,
) -> Option<SegmentCut> {
    if sample_rate_hz == 0 || samples.is_empty() {
        return None;
    }
    let rate = sample_rate_hz as usize;
    let total_secs = samples.len() as f32 / rate as f32;
    let flen = frame_len(sample_rate_hz);
    let mask = speech_mask(samples, sample_rate_hz);
    let has_speech = mask.iter().any(|s| *s);

    // Hard cap: cut regardless of what the speaker is doing.
    if total_secs >= cfg.max_secs {
        return Some(SegmentCut {
            take: samples.len(),
            has_speech,
        });
    }

    if total_secs < cfg.min_secs || !has_speech {
        return None;
    }

    // Trailing silence long enough to be a real pause rather than a gap
    // between words?
    let pause_frames = ((cfg.pause_secs * 1000.0) as usize / FRAME_MS).max(1);
    let trailing = mask.iter().rev().take_while(|s| !**s).count();
    if trailing < pause_frames {
        return None;
    }

    // Cut just after the last speech, keeping a little padding so the final
    // word is not clipped.
    let last_speech = mask.len() - trailing;
    let pad_frames = (PAD_MS / FRAME_MS).max(1);
    let take = ((last_speech + pad_frames) * flen).min(samples.len());
    Some(SegmentCut {
        take,
        has_speech: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(secs: f32, rate: u32) -> Vec<f32> {
        let n = (secs * rate as f32) as usize;
        (0..n)
            .map(|i| (i as f32 * 0.05).sin() * 0.3)
            .collect()
    }

    fn quiet(secs: f32, rate: u32) -> Vec<f32> {
        let n = (secs * rate as f32) as usize;
        (0..n).map(|i| (i as f32 * 0.01).sin() * 0.0005).collect()
    }

    const RATE: u32 = 16_000;

    #[test]
    fn waits_while_the_speaker_is_still_going() {
        let cfg = SegmentCutConfig::default();
        // 10s of speech, no pause yet: keep accumulating.
        assert_eq!(find_segment_cut(&tone(10.0, RATE), RATE, cfg), None);
    }

    #[test]
    fn cuts_after_a_real_pause() {
        let cfg = SegmentCutConfig::default();
        let mut audio = tone(8.0, RATE);
        audio.extend(quiet(3.5, RATE));
        let cut = find_segment_cut(&audio, RATE, cfg).expect("pause should end the segment");
        assert!(cut.has_speech);
        // Cut lands shortly after the speech, not at the end of the silence.
        let cut_secs = cut.take as f32 / RATE as f32;
        assert!(
            (8.0..9.0).contains(&cut_secs),
            "expected the cut just after speech, got {cut_secs}s"
        );
    }

    #[test]
    fn a_short_gap_between_words_is_not_a_cut() {
        let cfg = SegmentCutConfig::default();
        let mut audio = tone(8.0, RATE);
        audio.extend(quiet(0.6, RATE)); // sentence gap, not a pause
        audio.extend(tone(2.0, RATE));
        assert_eq!(find_segment_cut(&audio, RATE, cfg), None);
    }

    #[test]
    fn the_hard_cap_still_cuts_a_continuous_talker() {
        let cfg = SegmentCutConfig::default();
        let audio = tone(30.0, RATE);
        let cut = find_segment_cut(&audio, RATE, cfg).expect("cap must force a cut");
        assert_eq!(cut.take, audio.len());
        assert!(cut.has_speech);
    }

    #[test]
    fn the_cap_follows_the_configured_length() {
        let cfg = SegmentCutConfig {
            max_secs: 10.0,
            ..SegmentCutConfig::default()
        };
        assert!(find_segment_cut(&tone(10.0, RATE), RATE, cfg).is_some());
        assert_eq!(find_segment_cut(&tone(9.0, RATE), RATE, cfg), None);
    }

    #[test]
    fn silence_alone_never_cuts_early_and_is_flagged_at_the_cap() {
        let cfg = SegmentCutConfig::default();
        // Nobody spoke: no early cut...
        assert_eq!(find_segment_cut(&quiet(20.0, RATE), RATE, cfg), None);
        // ...and at the cap it is marked speechless so the caller can skip
        // sending it to a model that would invent words for it.
        let cut = find_segment_cut(&quiet(30.0, RATE), RATE, cfg).expect("cap cuts");
        assert!(!cut.has_speech, "silence must not be reported as speech");
    }

    #[test]
    fn a_brief_utterance_waits_for_the_minimum_length() {
        let cfg = SegmentCutConfig::default();
        let mut audio = tone(1.0, RATE);
        audio.extend(quiet(3.5, RATE)); // pause qualifies, total is under min
        assert_eq!(find_segment_cut(&audio, RATE, cfg), None);
    }
}
