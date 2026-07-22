pub mod settings;
pub mod synthesis;

pub use settings::{MeetingSettings, MeetingSettingsRepo};
pub use synthesis::{
    build_meeting_notes_request, build_meeting_title_request, format_transcript_for_synthesis,
    parse_meeting_notes_json, sanitize_meeting_title, strip_markdown_fence, ParsedMeetingNotes,
    MEETING_NOTES_PROMPT_VERSION, MEETING_TITLE_PROMPT_VERSION,
};
