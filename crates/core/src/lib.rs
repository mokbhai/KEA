pub mod dictation;
pub mod error;
pub mod log;
pub mod meetings;
pub mod resolve;
pub mod rewrite;
pub mod secrets;
pub mod store;
pub mod tts;

pub use store::conversations::{
    ConversationRepo, ConversationSummary, Message, MessageRole, NewConversation, NewMessage,
};
pub use store::meetings::{
    CaptureMode, Meeting, MeetingDetail, MeetingNotes, MeetingRepo, MeetingSegment, MeetingStatus,
    NewMeeting, NewSegment,
};
pub use tts::{TtsSettings, TtsSettingsRepo};

pub fn crate_name() -> &'static str {
    "kea-core"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_the_crate() {
        assert_eq!(crate_name(), "kea-core");
    }
}
