pub mod dictation;
pub mod error;
pub mod meetings;
pub mod store;
pub mod secrets;
pub mod log;
pub mod resolve;
pub mod rewrite;
pub mod tts;

pub use store::meetings::{
    Meeting, MeetingDetail, MeetingNotes, MeetingRepo, MeetingSegment, NewMeeting, NewSegment,
};
pub use store::conversations::{
    ConversationRepo, ConversationSummary, Message, NewConversation, NewMessage,
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
