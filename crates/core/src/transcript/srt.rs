//! SubRip (`.srt`) writer.

use kea_engines::traits::SttSegment;

use super::subtitle::{build_cues, format_timestamp, SubtitleOpts};

/// SRT's decimal separator. A dot here is the single most common reason a
/// generated `.srt` shows nothing in VLC.
const SRT_DECIMAL: char = ',';

/// Renders segments as SubRip.
///
/// Cue numbers start at 1 and increment with no gaps — they are assigned
/// after empty cues are dropped, which is why numbering lives here and not in
/// the cue builder. No `WEBVTT` header, no escaping: SRT has no markup rules,
/// and escaping `<` here would put a literal `&lt;` on screen.
pub fn to_srt(segments: &[SttSegment], opts: &SubtitleOpts) -> String {
    let cues = build_cues(segments, opts);
    let mut out = String::new();
    for (index, cue) in cues.iter().enumerate() {
        out.push_str(&(index + 1).to_string());
        out.push('\n');
        out.push_str(&format_timestamp(cue.start_ms, SRT_DECIMAL));
        out.push_str(" --> ");
        out.push_str(&format_timestamp(cue.end_ms, SRT_DECIMAL));
        out.push('\n');
        out.push_str(&cue.text());
        out.push_str("\n\n");
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
    fn two_cues_render_to_the_golden_string() {
        let out = to_srt(
            &[
                seg(0, 1_500, "Hello there."),
                seg(2_000, 4_000, "General Kenobi."),
            ],
            &SubtitleOpts::default(),
        );
        assert_eq!(
            out,
            "1\n00:00:00,000 --> 00:00:01,500\nHello there.\n\n\
             2\n00:00:02,000 --> 00:00:04,000\nGeneral Kenobi.\n\n"
        );
    }

    /// Asserted explicitly, in both directions, because the two formats differ by
    /// exactly this one character and a player's only symptom is silence.
    #[test]
    fn srt_uses_a_comma_and_never_a_dot() {
        let out = to_srt(&[seg(1_000, 2_000, "x")], &SubtitleOpts::default());
        assert!(out.contains("00:00:01,000"));
        assert!(!out.contains("00:00:01.000"));
    }

    #[test]
    fn numbering_starts_at_one_after_empty_cues_are_dropped() {
        let out = to_srt(
            &[seg(0, 1_000, "  "), seg(1_000, 2_000, "kept")],
            &SubtitleOpts::default(),
        );
        assert!(out.starts_with("1\n00:00:01,000"), "{out}");
        assert!(!out.contains("\n2\n"));
    }

    /// SRT has no markup rules. Escaping here would put `&lt;b&gt;` on screen.
    #[test]
    fn markup_in_cue_text_is_left_alone() {
        let out = to_srt(
            &[seg(0, 1_000, "<b>bold</b> & co")],
            &SubtitleOpts::default(),
        );
        assert!(out.contains("<b>bold</b> & co"), "{out}");
    }

    #[test]
    fn empty_input_is_an_empty_file() {
        assert_eq!(to_srt(&[], &SubtitleOpts::default()), "");
    }

    #[test]
    fn the_file_ends_with_a_trailing_newline() {
        let out = to_srt(&[seg(0, 1_000, "x")], &SubtitleOpts::default());
        assert!(out.ends_with('\n'));
        assert!(!out.contains('\r'), "no CRLF, and no BOM");
    }
}
