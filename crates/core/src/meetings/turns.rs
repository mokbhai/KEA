//! Where to cut one chunk of meeting audio so each piece is one speaker.
//!
//! The capture cuts a chunk at a pause of a second or more, or at the user's
//! segment length. A conversation hands over in a fraction of that, so a chunk
//! routinely holds several people, and labelling it by its majority voice put
//! the whole of it under one name. The diarizer finds the speaker turns inside
//! the chunk; this module turns its spans into the pieces to transcribe.
//!
//! Pure over the spans, so the whole decision is testable without a model.

use kea_infer::SpeakerSpan;

/// Shortest piece worth its own line.
///
/// Under about a second a clip transcribes badly — Whisper drops or invents
/// words with that little context — and a one-word interjection is better
/// folded into the turn around it than given a line it would garble.
pub const MIN_TURN_MS: u64 = 1_000;

/// One piece of a chunk, in milliseconds from the chunk's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnPlan {
    pub start_ms: u64,
    pub end_ms: u64,
    /// The diarizer's speaker index within this chunk only. It says which
    /// pieces are the same voice here; who that voice is across the meeting is
    /// decided by the caller from the piece's audio.
    pub local_speaker: u32,
}

/// The pieces to cut a chunk of `total_ms` into, or a single piece covering
/// all of it when there is nothing to split.
///
/// - Overlap goes to the speaker already talking: a piece has one speaker, and
///   the one who held the floor is the better guess for the shared stretch.
/// - Gaps between turns join the turn before, and the first turn starts at 0
///   and the last ends at `total_ms`, so no audio is dropped between pieces.
/// - Consecutive turns of the same speaker merge, and a turn shorter than
///   [`MIN_TURN_MS`] merges into its longer neighbour.
pub fn plan_turns(spans: &[SpeakerSpan], total_ms: u64) -> Vec<TurnPlan> {
    let whole = || {
        vec![TurnPlan {
            start_ms: 0,
            end_ms: total_ms,
            local_speaker: 0,
        }]
    };
    let mut sorted: Vec<&SpeakerSpan> = spans
        .iter()
        .filter(|s| s.end_ms > s.start_ms && s.start_ms < total_ms)
        .collect();
    sorted.sort_by_key(|s| (s.start_ms, s.end_ms));
    if sorted.is_empty() {
        return whole();
    }

    // Non-overlapping turns, in order.
    let mut turns: Vec<TurnPlan> = Vec::new();
    for span in sorted {
        let end = span.end_ms.min(total_ms);
        match turns.last_mut() {
            Some(last) if span.speaker == last.local_speaker && span.start_ms <= last.end_ms => {
                last.end_ms = last.end_ms.max(end);
            }
            Some(last) if span.start_ms < last.end_ms => {
                // Overlap: the earlier speaker keeps the shared stretch, and
                // the new speaker's turn starts where theirs ends.
                if end > last.end_ms {
                    let start = last.end_ms;
                    turns.push(TurnPlan {
                        start_ms: start,
                        end_ms: end,
                        local_speaker: span.speaker,
                    });
                }
            }
            _ => turns.push(TurnPlan {
                start_ms: span.start_ms,
                end_ms: end,
                local_speaker: span.speaker,
            }),
        }
    }

    // Close the gaps so the pieces tile the chunk.
    turns[0].start_ms = 0;
    for i in 1..turns.len() {
        turns[i - 1].end_ms = turns[i].start_ms;
    }
    if let Some(last) = turns.last_mut() {
        last.end_ms = total_ms;
    }

    // Merge until stable: same speaker side by side, or a piece too short to
    // stand alone, joins its neighbour.
    loop {
        let mut changed = false;
        let mut i = 0;
        while i + 1 < turns.len() {
            if turns[i].local_speaker == turns[i + 1].local_speaker {
                turns[i].end_ms = turns[i + 1].end_ms;
                turns.remove(i + 1);
                changed = true;
            } else {
                i += 1;
            }
        }
        let shortest = turns
            .iter()
            .enumerate()
            .filter(|(_, t)| t.end_ms - t.start_ms < MIN_TURN_MS)
            .min_by_key(|(_, t)| t.end_ms - t.start_ms)
            .map(|(i, _)| i);
        if let (Some(i), true) = (shortest, turns.len() > 1) {
            let len = |t: &TurnPlan| t.end_ms - t.start_ms;
            let into_previous = match (i.checked_sub(1), turns.get(i + 1)) {
                (Some(p), Some(next)) => len(&turns[p]) >= len(next),
                (Some(_), None) => true,
                _ => false,
            };
            if into_previous {
                turns[i - 1].end_ms = turns[i].end_ms;
            } else {
                turns[i + 1].start_ms = turns[i].start_ms;
            }
            turns.remove(i);
            changed = true;
        }
        if !changed {
            break;
        }
    }

    if turns.is_empty() {
        return whole();
    }
    turns
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

    fn ranges(turns: &[TurnPlan]) -> Vec<(u64, u64, u32)> {
        turns
            .iter()
            .map(|t| (t.start_ms, t.end_ms, t.local_speaker))
            .collect()
    }

    /// The reported case: two people inside one chunk become two pieces.
    #[test]
    fn a_hand_over_inside_a_chunk_splits_it() {
        let turns = plan_turns(&[span(200, 6_000, 0), span(6_400, 12_000, 1)], 12_500);
        assert_eq!(ranges(&turns), vec![(0, 6_400, 0), (6_400, 12_500, 1)]);
    }

    #[test]
    fn nothing_found_is_one_piece() {
        assert_eq!(ranges(&plan_turns(&[], 9_000)), vec![(0, 9_000, 0)]);
    }

    #[test]
    fn one_speaker_with_pauses_is_one_piece() {
        let turns = plan_turns(&[span(0, 3_000, 0), span(4_000, 8_000, 0)], 8_000);
        assert_eq!(ranges(&turns), vec![(0, 8_000, 0)]);
    }

    /// A one-word interjection is folded into the longer turn beside it.
    #[test]
    fn a_short_interjection_joins_its_longer_neighbour() {
        let turns = plan_turns(
            &[
                span(0, 5_000, 0),
                span(5_000, 5_400, 1),
                span(5_400, 7_000, 0),
            ],
            7_000,
        );
        assert_eq!(ranges(&turns), vec![(0, 7_000, 0)]);
    }

    #[test]
    fn overlap_goes_to_the_speaker_already_talking() {
        let turns = plan_turns(&[span(0, 5_000, 0), span(4_000, 9_000, 1)], 9_000);
        assert_eq!(ranges(&turns), vec![(0, 5_000, 0), (5_000, 9_000, 1)]);
    }

    #[test]
    fn a_turn_inside_another_is_absorbed() {
        let turns = plan_turns(&[span(0, 8_000, 0), span(2_000, 4_000, 1)], 8_000);
        assert_eq!(ranges(&turns), vec![(0, 8_000, 0)]);
    }

    #[test]
    fn three_speakers_in_a_row_stay_three_pieces() {
        let turns = plan_turns(
            &[
                span(0, 3_000, 0),
                span(3_000, 6_000, 1),
                span(6_000, 9_000, 2),
            ],
            9_000,
        );
        assert_eq!(
            ranges(&turns),
            vec![(0, 3_000, 0), (3_000, 6_000, 1), (6_000, 9_000, 2)]
        );
    }

    #[test]
    fn spans_past_the_chunk_are_clipped() {
        let turns = plan_turns(&[span(0, 3_000, 0), span(3_000, 20_000, 1)], 6_000);
        assert_eq!(ranges(&turns), vec![(0, 3_000, 0), (3_000, 6_000, 1)]);
    }
}
