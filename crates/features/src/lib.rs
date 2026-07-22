pub mod dictation;
pub mod feature;
pub mod registry;
pub mod demo;
pub mod meeting;
pub mod rewrite;
pub mod tts;
pub use dictation::{DictationFeature, run_dictation, run_dictation_with_storage};
pub use feature::{CapKind, CapSlot, Command, Feature};
pub use meeting::{
    drain_and_stop_meeting, ActiveMeeting, MeetingFeature, MeetingRunContext, MeetingSegmentEvent,
    run_meeting_poll_segment, run_meeting_start, run_meeting_stop, synthesize_meeting_notes,
    synthesize_meeting_title, transcribe_meeting_segment, transcribe_pcm_segment,
};
pub use registry::FeatureRegistry;
pub use rewrite::{
    ContentStorageOpts, RewriteFeature, run_rewrite, run_rewrite_with_storage,
};
pub use tts::{TtsFeature, run_tts, run_tts_synthesize};

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
