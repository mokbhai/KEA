pub mod attribution;
pub mod chat;
pub mod export;
pub mod interim;
pub mod notion;
pub mod settings;
pub mod synthesis;
pub mod voices;

pub use attribution::{attribute_segment, SpeakerChannel};
pub use chat::{
    build_chat_request, parse_chat_reply, ChatReply, ChatRole, ChatTurn, NotesProposal,
    CHAT_HISTORY_WINDOW,
};
pub use export::{markdown_file_name, meeting_to_markdown};
pub use interim::{
    should_run_interim, InterimCadence, MAX_CONSECUTIVE_INTERIM_FAILURES, MIN_INTERIM_GAP_SECS,
};
pub use notion::{
    append_children_payload, block_children_url, chunk_children, classify, create_page_payload,
    markdown_to_blocks, page_id, page_url, parse_page_id, NotionError, MAX_CHILDREN_PER_REQUEST,
    NOTION_API_VERSION, NOTION_PAGES_URL,
};

pub use settings::{MeetingSettings, MeetingSettingsRepo};
pub use synthesis::{
    build_interim_notes_request, build_meeting_notes_request, build_meeting_title_request,
    build_notes_repair_request, build_speaker_names_request, first_json_object,
    format_transcript_for_synthesis, parse_meeting_notes_json, parse_speaker_names,
    render_action_items_prose, sanitize_meeting_title, split_action_item_prose,
    strip_markdown_fence, ParsedActionItem, ParsedMeetingNotes, SpeakerNameProposal,
    MEETING_INTERIM_PROMPT_VERSION, MEETING_NOTES_PROMPT_VERSION, MEETING_TITLE_PROMPT_VERSION,
};
pub use voices::{
    embedding_windows, person_display_name, ReclusterResult, VoiceClusters, JOIN_SIM, MAX_CLUSTERS,
    MERGE_SIM, MIN_WINDOWS,
};
