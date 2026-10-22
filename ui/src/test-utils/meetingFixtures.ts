import type { MeetingActionItem, MeetingDetail as MeetingDetailData, MeetingNotes } from "../api";

/** A finished two-speaker meeting with three segments, the shape `get_meeting` returns. */
export function meetingDetail(over: Record<string, unknown> = {}): MeetingDetailData {
  return {
    meeting: {
      id: "m1",
      title: "Weekly Sync",
      started_at: "2026-09-19T10:00:00Z",
      ended_at: "2026-09-19T10:30:00Z",
      status: "completed",
      capture_mode: "mic_and_system",
      stt_engine_id: "whisper",
      llm_engine_id: "openai",
      error: null,
      title_source: "llm",
    },
    segments: [
      {
        id: 1,
        meeting_id: "m1",
        sequence: 0,
        start_offset_ms: 0,
        end_offset_ms: 5_000,
        text: "shall we start",
        speaker_key: "local",
      },
      {
        id: 2,
        meeting_id: "m1",
        sequence: 1,
        start_offset_ms: 5_000,
        end_offset_ms: 9_000,
        text: "yes go ahead",
        speaker_key: "remote",
      },
      {
        id: 3,
        meeting_id: "m1",
        sequence: 2,
        start_offset_ms: 9_000,
        end_offset_ms: 12_000,
        text: "crosstalk",
        speaker_key: "mixed",
      },
    ],
    notes: null,
    speakers: [
      { meeting_id: "m1", speaker_key: "local", display_name: "You", source: "channel" },
      { meeting_id: "m1", speaker_key: "remote", display_name: "Others", source: "channel" },
    ],
    action_items: [],
    can_undo: false,
    ...over,
  } as unknown as MeetingDetailData;
}

export const meetingNotes: MeetingNotes = {
  meeting_id: "m1",
  summary: "The launch slips a week.",
  decisions: "",
  action_items: "Send the deck — Priya (by Friday)",
  follow_ups: "",
  open_questions: "",
  prompt_version: "meeting-notes-v3",
  engine_id: "openai",
  model: "gpt-4o-mini",
  edited: false,
};

export const meetingActionItems: MeetingActionItem[] = [
  {
    id: 1,
    meeting_id: "m1",
    text: "Send the deck",
    owner: "Priya",
    due_hint: "by Friday",
    source_seq: 0,
    status: "open",
  },
  {
    id: 2,
    meeting_id: "m1",
    text: "Book the room",
    owner: null,
    due_hint: null,
    source_seq: null,
    status: "done",
  },
];
