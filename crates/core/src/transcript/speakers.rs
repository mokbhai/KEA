//! Attaching diarized speakers to transcript cues.
//!
//! The two passes disagree about where turns begin: the recognizer segments
//! on decoder state and the diarizer on acoustics, so their spans overlap
//! rather than align. Resolving that overlap is a pure function over two
//! lists of integers, which is why it is here and not inside either engine.

use kea_engines::traits::SttSegment;
use kea_infer::SpeakerSpan;

/// The label a cue is given when nothing overlaps it.
///
/// `None`, not `"unknown"`: a cue with no speaker reads exactly as it did
/// before diarization existed, which is what keeps an un-diarized transcript
/// and a partly-diarized one rendering the same way.
pub type SpeakerKey = Option<String>;

/// The stable key for a speaker index. `spk0`, `spk1`, …
///
/// Scoped to one recording by construction — there is no cross-recording
/// voiceprint library, and there deliberately will not be one: storing
/// embeddings against a named person, persisted across recordings, is a
/// biometric database and a separate product decision.
pub fn speaker_key(index: u32) -> String {
    format!("spk{index}")
}

/// A readable default name for a speaker key.
pub fn speaker_display_name(index: u32) -> String {
    format!("Speaker {}", index + 1)
}

/// Assigns each cue the speaker it spends the most time inside.
///
/// Majority by *overlapped duration*, not by the span the cue starts in: a
/// cue that begins in the last 100 ms of one turn and runs for four seconds
/// inside the next belongs to the next, and a start-time rule gets exactly
/// that case wrong at every speaker change — which is every case that
/// matters.
pub fn assign_speakers(segments: &[SttSegment], spans: &[SpeakerSpan]) -> Vec<SpeakerKey> {
    segments
        .iter()
        .map(|segment| {
            let mut best: Option<(u64, u32)> = None;
            for span in spans {
                let start = segment.start_ms.max(span.start_ms);
                let end = segment.end_ms.min(span.end_ms);
                let overlap = end.saturating_sub(start);
                if overlap == 0 {
                    continue;
                }
                // Strictly greater, so equal overlaps keep the earlier
                // speaker rather than flipping on list order.
                if best.is_none_or(|(best_overlap, _)| overlap > best_overlap) {
                    best = Some((overlap, span.speaker));
                }
            }
            best.map(|(_, speaker)| speaker_key(speaker))
        })
        .collect()
}

/// The distinct speakers present in an assignment, in first-appearance order,
/// as `(key, display_name)`.
///
/// First-appearance rather than numeric order, because that is the order a
/// reader meets them in and therefore the order a legend should list them.
pub fn speaker_legend(spans: &[SpeakerSpan]) -> Vec<(String, String)> {
    let mut seen: Vec<u32> = Vec::new();
    for span in spans {
        if !seen.contains(&span.speaker) {
            seen.push(span.speaker);
        }
    }
    seen.into_iter()
        .map(|index| (speaker_key(index), speaker_display_name(index)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(start_ms: u64, end_ms: u64, speaker: u32) -> SpeakerSpan {
        SpeakerSpan {
            start_ms,
            end_ms,
            speaker,
        }
    }

    #[test]
    fn a_cue_takes_the_speaker_it_overlaps_most() {
        let segments = [
            SttSegment::new(0, 2_000, "first"),
            SttSegment::new(2_000, 4_000, "second"),
        ];
        let spans = [span(0, 2_100, 0), span(2_100, 5_000, 1)];
        assert_eq!(
            assign_speakers(&segments, &spans),
            vec![Some("spk0".into()), Some("spk1".into())]
        );
    }

    /// The case a start-time rule gets wrong, and it is the case at every
    /// speaker change: the cue begins in one turn and lives in the next.
    #[test]
    fn a_cue_straddling_a_turn_goes_to_where_it_spends_its_time() {
        let segments = [SttSegment::new(1_900, 6_000, "mostly the second speaker")];
        let spans = [span(0, 2_000, 0), span(2_000, 6_000, 1)];
        assert_eq!(
            assign_speakers(&segments, &spans),
            vec![Some("spk1".into())]
        );
    }

    /// No overlap is `None`, which renders exactly as an un-diarized cue.
    #[test]
    fn a_cue_with_no_overlap_gets_no_label() {
        let segments = [SttSegment::new(10_000, 11_000, "after everyone left")];
        assert_eq!(assign_speakers(&segments, &[span(0, 5_000, 0)]), vec![None]);
        assert_eq!(assign_speakers(&segments, &[]), vec![None]);
    }

    /// A zero-length cue overlaps nothing, and must not divide by its own
    /// duration or claim the first span by default.
    #[test]
    fn a_zero_length_cue_is_unlabelled_rather_than_arbitrary() {
        let segments = [SttSegment::new(1_000, 1_000, "")];
        assert_eq!(assign_speakers(&segments, &[span(0, 5_000, 3)]), vec![None]);
    }

    #[test]
    fn the_legend_lists_speakers_in_the_order_they_are_first_heard() {
        let spans = [
            span(0, 1_000, 2),
            span(1_000, 2_000, 0),
            span(2_000, 3_000, 2),
        ];
        assert_eq!(
            speaker_legend(&spans),
            vec![
                ("spk2".to_string(), "Speaker 3".to_string()),
                ("spk0".to_string(), "Speaker 1".to_string()),
            ]
        );
        assert!(speaker_legend(&[]).is_empty());
    }
}
