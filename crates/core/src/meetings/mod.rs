pub mod attribution;
pub mod export;
pub mod interim;
pub mod settings;
pub mod synthesis;

pub use attribution::{attribute_segment, SpeakerChannel};
pub use export::{markdown_file_name, meeting_to_markdown};
pub use interim::{
    should_run_interim, InterimCadence, MAX_CONSECUTIVE_INTERIM_FAILURES, MIN_INTERIM_GAP_SECS,
};
pub use settings::{MeetingSettings, MeetingSettingsRepo};
pub use synthesis::{
    build_interim_notes_request, build_meeting_notes_request, build_meeting_title_request,
    build_notes_repair_request, first_json_object, format_transcript_for_synthesis,
    parse_meeting_notes_json, render_action_items_prose, sanitize_meeting_title,
    split_action_item_prose, strip_markdown_fence, ParsedActionItem, ParsedMeetingNotes,
    MEETING_INTERIM_PROMPT_VERSION, MEETING_NOTES_PROMPT_VERSION, MEETING_TITLE_PROMPT_VERSION,
};
