//! Turning engine segments into subtitle cues.
//!
//! Pure: no pool, no file, no engine. Every decision a subtitle format forces
//! — how long a line may be, what happens when two cues overlap, whether an
//! empty cue keeps its number — is made here once, so SRT and VTT differ only
//! in how they *print* the same cue list. The two formats disagree about the
//! decimal separator and about escaping, and nothing else; anywhere else they
//! diverged would be a bug in one of them.

use kea_engines::traits::SttSegment;

/// Formatting rules a subtitle file is written under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubtitleOpts {
    /// Longest rendered line, in `char`s.
    ///
    /// Counted in `char`s rather than grapheme clusters: there is no
    /// `unicode-segmentation` crate in the tree and pulling one in to make a
    /// flag emoji count as one column is not worth it. The consequence is
    /// that a line of combining marks wraps early, which is a cosmetic error
    /// in a format whose players all reflow anyway.
    pub max_line_chars: usize,
    /// Most lines in one cue. A third line is past what a viewer reads in the
    /// time a cue is on screen.
    pub max_lines: usize,
    /// Shortest a cue may be shown. Below roughly this, a cue flashes.
    pub min_cue_ms: u64,
    /// Longest a cue may be shown before it is cut.
    pub max_cue_ms: u64,
}

impl Default for SubtitleOpts {
    fn default() -> Self {
        Self {
            max_line_chars: 42,
            max_lines: 2,
            min_cue_ms: 700,
            max_cue_ms: 7_000,
        }
    }
}

/// One rendered cue: a time span and the lines to show in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub lines: Vec<String>,
}

impl Cue {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

/// `HH:MM:SS<sep>mmm`, zero-padded, hours always two digits.
///
/// `sep` is the whole difference between an SRT timestamp and a VTT one:
/// SRT writes a comma, VTT a dot, and swapping them produces a file every
/// player rejects — silently, by showing no subtitles at all.
pub fn format_timestamp(ms: u64, sep: char) -> String {
    let total_secs = ms / 1000;
    let millis = ms % 1000;
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}{sep}{millis:03}")
}

/// Wraps `text` into lines of at most `max_chars`, breaking on whitespace.
///
/// Text with no break opportunity — CJK, a URL — is cut by `char` count
/// instead of being allowed to overflow: an unbreakable 400-character line
/// renders off the edge of every player.
fn wrap_lines(text: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 {
        return vec![text.to_string()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if word_len > max_chars {
            // No break opportunity inside it, so cut by char count. Whatever
            // is already on the line is flushed first so the cut starts clean.
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
            }
            let chars: Vec<char> = word.chars().collect();
            for piece in chars.chunks(max_chars) {
                lines.push(piece.iter().collect());
            }
            // The last piece may have room left; keep filling it.
            if let Some(last) = lines.pop() {
                current_len = last.chars().count();
                current = last;
            }
            continue;
        }
        let needed = if current.is_empty() {
            word_len
        } else {
            word_len + 1
        };
        if current_len + needed > max_chars && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_len = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            current_len += 1;
        }
        current.push_str(word);
        current_len += word_len;
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Builds the cue list a writer prints.
///
/// The order of the passes is the contract, not an implementation detail:
///
/// 1. empty text is dropped, *before* anything is numbered — an empty cue
///    that kept its number would leave a gap in the sequence, which some
///    players treat as a truncated file;
/// 2. a cue too long to render splits into consecutive cues, its duration
///    divided by character count, so the split is invisible in the timing;
/// 3. each cue is clamped to the min/max show time;
/// 4. only then is an overlap with the next cue resolved, because steps 2
///    and 3 can both create one.
pub fn build_cues(segments: &[SttSegment], opts: &SubtitleOpts) -> Vec<Cue> {
    let mut kept: Vec<SttSegment> = segments
        .iter()
        .filter(|s| !s.text.trim().is_empty())
        .cloned()
        .collect();
    kept.sort_by_key(|s| (s.start_ms, s.end_ms));

    let mut cues: Vec<Cue> = Vec::with_capacity(kept.len());
    for seg in kept {
        let text = seg.text.trim();
        let lines = wrap_lines(text, opts.max_line_chars);
        let end = seg.end_ms.max(seg.start_ms);
        if lines.len() <= opts.max_lines.max(1) {
            cues.push(Cue {
                start_ms: seg.start_ms,
                end_ms: end,
                lines,
            });
            continue;
        }
        cues.extend(split_cue(seg.start_ms, end, &lines, opts.max_lines.max(1)));
    }

    for cue in &mut cues {
        let start = cue.start_ms;
        let mut end = cue.end_ms.max(start);
        if end.saturating_sub(start) < opts.min_cue_ms {
            end = start + opts.min_cue_ms;
        }
        if end.saturating_sub(start) > opts.max_cue_ms {
            end = start + opts.max_cue_ms;
        }
        cue.end_ms = end;
    }

    // Trailing pass, so a min-clamp above cannot leave a cue overlapping its
    // neighbour. A cue whose neighbour starts before it does keeps a
    // zero-length span rather than a negative one.
    for i in 0..cues.len().saturating_sub(1) {
        let next_start = cues[i + 1].start_ms;
        if cues[i].end_ms > next_start {
            cues[i].end_ms = next_start.max(cues[i].start_ms);
        }
    }

    cues
}

/// Splits one over-long cue into consecutive cues of `max_lines` lines each,
/// dividing the span in proportion to the characters in each part.
///
/// Proportional rather than equal: two lines of dialogue and a one-word
/// exclamation are not on screen for the same time, and an equal split makes
/// the short one linger while the long one races.
fn split_cue(start_ms: u64, end_ms: u64, lines: &[String], max_lines: usize) -> Vec<Cue> {
    let groups: Vec<Vec<String>> = lines
        .chunks(max_lines)
        .map(|chunk| chunk.to_vec())
        .collect();
    let weights: Vec<usize> = groups
        .iter()
        .map(|g| g.iter().map(|l| l.chars().count()).sum::<usize>().max(1))
        .collect();
    let total_weight: usize = weights.iter().sum();
    let span = end_ms.saturating_sub(start_ms);

    let mut out = Vec::with_capacity(groups.len());
    let mut cursor = start_ms;
    let mut used = 0usize;
    for (i, group) in groups.into_iter().enumerate() {
        used += weights[i];
        // Derived from the running total rather than accumulated, so rounding
        // cannot drift and the last cue always lands exactly on `end_ms`.
        let next = if i + 1 == weights.len() {
            end_ms
        } else {
            start_ms + (span as u128 * used as u128 / total_weight as u128) as u64
        };
        out.push(Cue {
            start_ms: cursor,
            end_ms: next.max(cursor),
            lines: group,
        });
        cursor = next;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: u64, end: u64, text: &str) -> SttSegment {
        SttSegment::new(start, end, text)
    }

    #[test]
    fn timestamps_pad_hours_even_under_an_hour() {
        assert_eq!(format_timestamp(0, ','), "00:00:00,000");
        assert_eq!(format_timestamp(999, ','), "00:00:00,999");
        // The off-by-one every naive implementation makes: 1000 must roll into
        // the seconds field, never print as ",1000".
        assert_eq!(format_timestamp(1_000, ','), "00:00:01,000");
        assert_eq!(format_timestamp(3_600_000, ','), "01:00:00,000");
        assert_eq!(format_timestamp(3_661_001, '.'), "01:01:01.001");
    }

    #[test]
    fn wrapping_breaks_on_whitespace_only() {
        let lines = wrap_lines("the quick brown fox jumps over the lazy dog", 15);
        assert!(lines.iter().all(|l| l.chars().count() <= 15), "{lines:?}");
        assert_eq!(
            lines.join(" "),
            "the quick brown fox jumps over the lazy dog"
        );
    }

    /// CJK has no break opportunity, so the only honest answer is to cut by
    /// char count rather than let the line run off the screen.
    #[test]
    fn text_with_no_spaces_is_cut_by_char_count() {
        let lines = wrap_lines(&"あ".repeat(10), 4);
        assert_eq!(lines, vec!["ああああ", "ああああ", "ああ"]);
    }

    #[test]
    fn an_empty_cue_is_dropped_and_the_rest_renumber() {
        let cues = build_cues(
            &[
                seg(0, 1_000, "   "),
                seg(1_000, 2_000, "first real line"),
                seg(2_000, 3_000, ""),
            ],
            &SubtitleOpts::default(),
        );
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text(), "first real line");
    }

    #[test]
    fn an_overlapping_pair_is_clamped_to_the_next_start() {
        let cues = build_cues(
            &[seg(0, 5_000, "one"), seg(2_000, 4_000, "two")],
            &SubtitleOpts::default(),
        );
        assert_eq!(cues[0].end_ms, 2_000);
        assert_eq!(cues[1].start_ms, 2_000);
    }

    #[test]
    fn short_cues_are_held_and_long_cues_are_cut() {
        let opts = SubtitleOpts::default();
        let cues = build_cues(&[seg(0, 50, "blink"), seg(10_000, 30_000, "epic")], &opts);
        assert_eq!(cues[0].end_ms, opts.min_cue_ms);
        assert_eq!(cues[1].end_ms - cues[1].start_ms, opts.max_cue_ms);
    }

    /// The split has to be invisible in the timing: the parts must still
    /// cover exactly the span the engine reported.
    #[test]
    fn a_long_cue_splits_into_cues_whose_durations_sum_to_the_original() {
        // Six 20-character words wrap to three 42-column lines, which is one
        // more than a cue may hold — so it has to become two cues.
        let text = ["a", "b", "c", "d", "e", "f"]
            .map(|c| c.repeat(20))
            .join(" ");
        assert!(text.chars().count() >= 90);
        let opts = SubtitleOpts::default();
        let cues = build_cues(&[seg(1_000, 7_000, &text)], &opts);
        assert!(cues.len() >= 2, "{cues:?}");
        assert_eq!(cues[0].start_ms, 1_000);
        assert_eq!(cues[cues.len() - 1].end_ms, 7_000);
        let covered: u64 = cues
            .iter()
            .map(|c| c.end_ms.saturating_sub(c.start_ms))
            .sum();
        assert_eq!(covered, 6_000, "the split must not lose or invent time");
        for cue in &cues {
            assert!(cue.lines.len() <= opts.max_lines);
        }
    }

    #[test]
    fn segments_are_sorted_before_they_are_numbered() {
        let cues = build_cues(
            &[seg(5_000, 6_000, "later"), seg(1_000, 2_000, "earlier")],
            &SubtitleOpts::default(),
        );
        assert_eq!(cues[0].text(), "earlier");
        assert_eq!(cues[1].text(), "later");
    }

    #[test]
    fn a_segment_ending_before_it_starts_yields_no_negative_span() {
        let cues = build_cues(&[seg(5_000, 1_000, "backwards")], &SubtitleOpts::default());
        assert!(cues[0].end_ms >= cues[0].start_ms);
    }
}
