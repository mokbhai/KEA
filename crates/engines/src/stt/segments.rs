//! Turning a backend's timing into [`SttSegment`]s.
//!
//! One module rather than a conversion at each engine: whisper, the sherpa
//! transducer and the hosted endpoint all report timing in a different unit,
//! and the only thing the rest of the app should ever see is milliseconds
//! relative to the buffer that was handed in.

use crate::traits::{SttSegment, Transcript};

/// Converts a local (`kea-infer`) result into the engine-layer transcript.
///
/// The two shapes are deliberately separate types — the crate dependency runs
/// engines -> infer — so this is the one place the field-for-field copy lives.
pub fn from_infer(result: kea_infer::SttResult) -> Transcript {
    Transcript {
        text: result.text,
        segments: result
            .segments
            .into_iter()
            .map(|s| SttSegment {
                start_ms: s.start_ms,
                end_ms: s.end_ms.max(s.start_ms),
                text: s.text.trim().to_string(),
            })
            .filter(|s| !s.text.is_empty())
            .collect(),
    }
}

/// Reads OpenAI's `verbose_json` segment array, defensively.
///
/// Every field here is optional in practice. A user-configured
/// OpenAI-*compatible* base URL frequently ignores `response_format` and
/// answers with a bare `{"text": …}`; some answer with `segments` whose
/// members lack `start`/`end`. None of that is an error — a transcript with
/// no timing is still a transcript, and the chunk driver supplies
/// chunk-granularity cues on top. Erroring here would break dictation
/// against every non-OpenAI endpoint for a feature dictation does not use.
pub fn from_openai_verbose(parsed: &serde_json::Value) -> Vec<SttSegment> {
    let Some(items) = parsed.get("segments").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let text = item
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if text.is_empty() {
            continue;
        }
        // Seconds as a float on the wire, milliseconds everywhere here.
        let start_ms = seconds_to_ms(item.get("start"));
        let end_ms = seconds_to_ms(item.get("end")).max(start_ms);
        out.push(SttSegment {
            start_ms,
            end_ms,
            text: text.to_string(),
        });
    }
    out
}

fn seconds_to_ms(value: Option<&serde_json::Value>) -> u64 {
    value
        .and_then(|v| v.as_f64())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .map(|v| (v * 1000.0).round() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn infer_segments_are_trimmed_and_empties_dropped() {
        let out = from_infer(kea_infer::SttResult {
            text: "Hello world.".into(),
            segments: vec![
                kea_infer::TimedSegment {
                    start_ms: 0,
                    end_ms: 900,
                    text: " Hello".into(),
                },
                kea_infer::TimedSegment {
                    start_ms: 900,
                    end_ms: 950,
                    text: "  ".into(),
                },
                // An engine that reports end before start must not produce a
                // negative-length cue downstream.
                kea_infer::TimedSegment {
                    start_ms: 1_000,
                    end_ms: 100,
                    text: "world.".into(),
                },
            ],
        });
        assert_eq!(out.text, "Hello world.");
        assert_eq!(out.segments.len(), 2);
        assert_eq!(out.segments[0].text, "Hello");
        assert_eq!(out.segments[1].start_ms, 1_000);
        assert_eq!(out.segments[1].end_ms, 1_000);
    }

    #[test]
    fn verbose_json_segments_convert_seconds_to_ms() {
        let parsed = json!({
            "text": "one two",
            "segments": [
                {"start": 0.0, "end": 1.24, "text": " one"},
                {"start": 1.24, "end": 2.5, "text": " two"}
            ]
        });
        let segs = from_openai_verbose(&parsed);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].end_ms, 1_240);
        assert_eq!(segs[1].text, "two");
    }

    /// The case that keeps every OpenAI-compatible endpoint working: a bare
    /// `{"text": …}` body is not an error, it is a transcript with no timing.
    #[test]
    fn a_body_without_segments_yields_no_segments() {
        assert!(from_openai_verbose(&json!({"text": "hi"})).is_empty());
        assert!(from_openai_verbose(&json!({"segments": "not an array"})).is_empty());
        assert!(from_openai_verbose(&json!({"segments": []})).is_empty());
    }

    #[test]
    fn malformed_bounds_fall_back_to_zero_rather_than_erroring() {
        let segs = from_openai_verbose(&json!({
            "segments": [
                {"text": "no bounds"},
                {"start": "x", "end": -4.0, "text": "bad bounds"},
                {"start": 1.0, "end": 2.0, "text": "   "}
            ]
        }));
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].start_ms, 0);
        assert_eq!(segs[1].end_ms, 0);
    }
}
