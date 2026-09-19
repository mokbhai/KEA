//! Transcripts of audio *files*: the subtitle writers and the chunked
//! transcription job that feeds them.
//!
//! Core rather than the engine or feature layer for the same reason
//! `meetings::synthesis` is here — these are pure formatters over a value
//! type, testable without a pool, a file or an engine.

pub mod job;
pub mod speakers;
pub mod srt;
pub mod store;
pub mod subtitle;
pub mod vtt;

pub use job::{
    plan_chunks, transcribe_chunks, ChunkSpan, SilentSink, TranscribeOutcome, TranscribeSink,
    DEFAULT_CHUNK_SECS,
};
pub use speakers::{
    assign_speakers, speaker_display_name, speaker_key, speaker_legend, SpeakerKey,
};
pub use srt::to_srt;
pub use store::{
    segments_from_rows, NewTranscript, TranscriptDetail, TranscriptRepo, TranscriptRow,
    TranscriptSegmentRow, TranscriptStatus,
};
pub use subtitle::{build_cues, format_timestamp, Cue, SubtitleOpts};
pub use vtt::to_vtt;

/// The subtitle formats the app can export.
///
/// An enum with `as_str`/`from_str` at the IPC and filename edges, the shape
/// `MeetingStatus` and `CaptureMode` use: the extension, the wire string and
/// the writer are one decision, and spelling them apart is how an export
/// writes VTT content into a `.srt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubtitleFormat {
    Srt,
    Vtt,
}

impl SubtitleFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            SubtitleFormat::Srt => "srt",
            SubtitleFormat::Vtt => "vtt",
        }
    }

    /// The file extension, which is also the wire string. One function, so
    /// the two can never drift.
    pub fn extension(&self) -> &'static str {
        self.as_str()
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "srt" => Some(SubtitleFormat::Srt),
            "vtt" => Some(SubtitleFormat::Vtt),
            _ => None,
        }
    }

    /// Renders segments in this format. The one dispatch point, so a new
    /// format is an arm here rather than a `match` at each caller.
    pub fn render(
        &self,
        segments: &[kea_engines::traits::SttSegment],
        opts: &SubtitleOpts,
    ) -> String {
        match self {
            SubtitleFormat::Srt => to_srt(segments, opts),
            SubtitleFormat::Vtt => to_vtt(segments, opts),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_engines::traits::SttSegment;

    #[test]
    fn the_format_spelling_is_the_extension_and_the_wire_string() {
        for (format, text) in [(SubtitleFormat::Srt, "srt"), (SubtitleFormat::Vtt, "vtt")] {
            assert_eq!(format.as_str(), text);
            assert_eq!(format.extension(), text);
            assert_eq!(SubtitleFormat::from_str(text), Some(format));
            assert_eq!(
                serde_json::to_string(&format).unwrap(),
                format!("\"{text}\"")
            );
        }
        assert_eq!(SubtitleFormat::from_str("ass"), None);
    }

    /// Dispatching through the enum is what keeps an export from writing VTT
    /// bytes into a file named `.srt`.
    #[test]
    fn render_picks_the_writer_that_matches_the_extension() {
        let segs = [SttSegment::new(0, 1_000, "hi")];
        let opts = SubtitleOpts::default();
        assert!(SubtitleFormat::Srt
            .render(&segs, &opts)
            .starts_with("1\n00:00:00,000"));
        assert!(SubtitleFormat::Vtt
            .render(&segs, &opts)
            .starts_with("WEBVTT"));
    }
}
