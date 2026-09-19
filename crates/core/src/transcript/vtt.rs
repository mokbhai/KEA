//! WebVTT (`.vtt`) writer.

use kea_engines::traits::SttSegment;

use super::subtitle::{build_cues, format_timestamp, SubtitleOpts};

/// WebVTT's decimal separator. See [`crate::transcript::srt`] for the other one.
const VTT_DECIMAL: char = '.';

/// Escapes the three characters WebVTT gives markup meaning to.
///
/// `&` first, or the ampersands introduced by the other two get escaped a
/// second time and `<` renders as `&amp;lt;`. This is the direction people
/// get backwards: SRT escapes nothing, VTT escapes these three.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Renders segments as WebVTT.
///
/// Cue identifiers are optional in the format and omitted here: they are only
/// useful for styling a specific cue from CSS, which nothing in KEA does, and
/// a numeric identifier on every cue is what makes a `.vtt` look like a
/// mis-renamed `.srt`.
pub fn to_vtt(segments: &[SttSegment], opts: &SubtitleOpts) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for cue in build_cues(segments, opts) {
        out.push_str(&format_timestamp(cue.start_ms, VTT_DECIMAL));
        out.push_str(" --> ");
        out.push_str(&format_timestamp(cue.end_ms, VTT_DECIMAL));
        out.push('\n');
        out.push_str(&escape(&cue.text()));
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
        let out = to_vtt(
            &[
                seg(0, 1_500, "Hello there."),
                seg(2_000, 4_000, "General Kenobi."),
            ],
            &SubtitleOpts::default(),
        );
        assert_eq!(
            out,
            "WEBVTT\n\n\
             00:00:00.000 --> 00:00:01.500\nHello there.\n\n\
             00:00:02.000 --> 00:00:04.000\nGeneral Kenobi.\n\n"
        );
    }

    #[test]
    fn vtt_uses_a_dot_and_never_a_comma() {
        let out = to_vtt(&[seg(1_000, 2_000, "x")], &SubtitleOpts::default());
        assert!(out.contains("00:00:01.000"));
        assert!(!out.contains("00:00:01,000"));
    }

    /// The classic bug, pinned in the direction that matters: VTT escapes,
    /// SRT does not.
    #[test]
    fn markup_and_ampersands_are_escaped_once() {
        let out = to_vtt(&[seg(0, 1_000, "<b>a</b> & b")], &SubtitleOpts::default());
        assert!(out.contains("&lt;b&gt;a&lt;/b&gt; &amp; b"), "{out}");
        assert!(!out.contains("&amp;lt;"), "an ampersand was escaped twice");
    }

    #[test]
    fn empty_input_is_a_header_and_nothing_else() {
        assert_eq!(to_vtt(&[], &SubtitleOpts::default()), "WEBVTT\n\n");
    }

    #[test]
    fn no_cue_identifiers_are_written() {
        let out = to_vtt(
            &[seg(0, 1_000, "one"), seg(2_000, 3_000, "two")],
            &SubtitleOpts::default(),
        );
        for line in out.lines() {
            assert!(
                line.parse::<u32>().is_err(),
                "a bare number is a cue identifier: {line}"
            );
        }
    }
}
