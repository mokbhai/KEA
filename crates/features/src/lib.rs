pub mod demo;
pub mod dictation;
pub mod feature;
pub mod meeting;
pub mod registry;
pub mod rewrite;
pub mod transcribe;
pub mod tts;
pub use dictation::{run_dictation, run_dictation_with_storage, DictationFeature};
pub use feature::{ActionGuard, CapKind, CapSlot, Command, Feature, ProfileOverrides};
pub use meeting::{
    drain_and_stop_meeting, run_meeting_poll_segment, run_meeting_start, run_meeting_stop,
    synthesize_meeting_notes, synthesize_meeting_title, transcribe_meeting_segment,
    transcribe_pcm_segment, ActiveMeeting, MeetingFeature, MeetingRunContext, MeetingSegmentEvent,
};
pub use registry::FeatureRegistry;
pub use rewrite::{
    run_rewrite, run_rewrite_with_storage, ContentStorageOpts, RewriteFeature, RewriteOutcome,
};
pub use transcribe::{TranscribeFeature, TRANSCRIBE_FEATURE_ID};
pub use tts::{run_tts, run_tts_synthesize, TtsFeature};

pub fn crate_name() -> &'static str {
    "kea-features"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_the_crate() {
        assert_eq!(crate_name(), "kea-features");
    }
}
