# Design

## Context

- **`ui/src/pages/MeetingsPage.tsx`** (~980 lines) renders one column, in this order: Behavior settings, "Try it" (record controls), "Notes so far", "Live transcript", "Past meetings" (a list), and the selected meeting (`MeetingDetail`).
  - The selection is `selectedId`/`detail` state, loaded with `getMeeting`.
  - The live state arrives through `onMeetingSegment`, `onMeetingNotes` and `onMeetingNotesError`.
- **`MeetingDetail.tsx`** (~500 lines) renders the title, speakers, notes, action items and transcript.
- **Storage:**
  - `meeting_notes` is one row per meeting, written by `persist_notes` (interim passes and the final pass). Notes have no user-edit path at all.
  - `conversations` (`feature_id`, engine, model) and `messages` (`role`, `content`) already exist and are used by the rewrite History page (`crates/core/src/store/conversations.rs`, `rewrite/history.rs`).
- **LLM calls for meetings** go through `meetings_llm`, which resolves the `meetings` binding, and `record_meeting_usage`.
- **Transcript formatting for prompts:** `format_transcript_for_synthesis` (with speaker names and `[mm:ss-mm:ss]` stamps) and the 80,000-character cap.

## Goals / Non-Goals

**Goals:**
- The page is restructured with no behaviour change to recording, settings, notes generation or export.
- The chat reuses the existing transcript formatting, LLM resolution, usage ledger, tolerant JSON parsing, and the conversations tables.
- The notes can never change without an explicit user action, and every change can be undone.

**Non-Goals:**
- **Streaming chat replies.** `LlmEngine::complete` is whole-reply. The tab shows a pending bubble.
- **Router-based URLs** for the selected meeting or tab. Selection survives within the app session through page-level state lifted to the app shell.
- **Retrieval or embeddings** for transcripts over the cap.

## Decisions

### 1. Split `MeetingsPage` into a shell and five components

New components in `ui/src/components/meetings/`:
- `MeetingList`: grouping, search, the pinned live row.
- `MeetingPane`: the title bar and tabs.
- `SummaryTab`, `TranscriptTab` and `AskAiTab`.
- `MeetingSettingsPanel`: the slide-over, holding the existing settings JSX moved verbatim.

`MeetingDetail.tsx` is broken up: its notes, action-item and speaker parts become `SummaryTab`, and its transcript part becomes `TranscriptTab`.

The page shell keeps today's state hooks: settings, live events and selection. It lays out a CSS grid with a ~280 px list column and a flexible detail column. Below ~720 px it shows one pane at a time with Back (spec: narrow window).

**Why a split rather than restyling in place:** the file mixes three concerns, and both the tab and the slide-over boundaries need their own state (scroll position, open/closed, focus return). Moving the settings JSX verbatim keeps the review diff legible, and it keeps the existing `MeetingsPage.test.tsx` settings assertions valid with only selector updates.

**Selection memory:** `selectedMeetingId` is lifted into the existing app-shell context that already persists the current page, so returning to Meetings restores it. The default selection is the recording meeting, else `list[0]`.

**Live view:** when the selected meeting is the recording one, `SummaryTab` and `TranscriptTab` render from the live event state instead of `getMeeting`. On `meeting:stopped` or completion, the pane refetches the detail. The "Try it" section is deleted, and Record/Stop moves to the header.

### 2. Search in SQL, grouping in the UI

`MeetingRepo::search(query, limit)` runs a `LIKE` over `meetings.title`, `meeting_notes.summary`, `decisions`, `action_items`, `follow_ups` and `open_questions`, case-insensitively. SQLite's `LIKE` is ASCII case-insensitive, and that is accepted. The query is escaped for `%` and `_`.

It is exposed as a `search_meetings` command, debounced 200 ms in the UI. An empty query uses the existing list.

Grouping (Today / Yesterday / This week / Earlier) is computed in the UI from `started_at` in local time. A duration comes from `ended_at - started_at`.

Alternative considered: SQLite full-text search. It needs a virtual table, triggers and a backfill, which is too much for a list that rarely exceeds a few hundred rows.

### 3. Storage: migration `0009_meeting_workspace.sql`

```sql
ALTER TABLE conversations ADD COLUMN meeting_id TEXT REFERENCES meetings(id) ON DELETE CASCADE;
CREATE INDEX idx_conversations_meeting ON conversations (meeting_id, id);
ALTER TABLE messages ADD COLUMN meta TEXT;            -- JSON: proposal + its state
ALTER TABLE meeting_notes ADD COLUMN edited INTEGER NOT NULL DEFAULT 0;
CREATE TABLE meeting_notes_versions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id  TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    notes_json  TEXT NOT NULL,                          -- the version replaced
    edited      INTEGER NOT NULL,                       -- its edited mark
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_notes_versions_meeting ON meeting_notes_versions (meeting_id, id);
```

**Meeting chats** are conversations with `feature_id = 'meetings'` and a `meeting_id`. Reusing the tables keeps one message model, and the cascade satisfies "deleting a meeting deletes its chat". The rewrite History query gains `WHERE meeting_id IS NULL`, so meeting chats don't appear on the History page; this is a task with a test. One conversation per meeting is created lazily on the first message. "Clear chat" deletes it.

**`messages.meta`** holds `{"proposal": ParsedMeetingNotes-shaped, "state": "pending"|"applied"|"discarded"}` on an assistant message that proposed an edit. It is JSON in one column because only the UI reads it back and it has one writer.

**Undo stack.**
- Every user change (an applied proposal or a direct edit) first inserts the current notes, with their `edited` flag, into `meeting_notes_versions`, then writes the new notes with `edited = 1`, in one transaction.
- Undo pops the newest version and restores it, `edited` flag included. Restoring the generated version therefore clears the mark (spec).
- The stack is capped at 50 versions per meeting, oldest dropped. That bounds growth for a user who iterates many times.

The existing `persist_notes` (generation) does not touch the versions table: generation isn't a user change. While recording, user changes are refused (decision 6), so generation and user versions never interleave.

**Applying a proposal** writes the summary, decisions, follow-ups and open questions from the proposal. `action_items` is re-rendered from the rows with `render_action_items_prose`, so rows stay the source of truth: the spec says the rows are changed only by the user, through the existing checkbox and add-item controls.

### 4. The chat turn: one prompt, a tolerant JSON reply

`kea_core::meetings::chat`, pure:
- `build_chat_request(notes, action_items, transcript, history, user_message, allow_edits) -> (LlmRequest, truncated: bool)`. It:
  - reuses `format_transcript_for_synthesis` for the transcript, and its cap, reporting `truncated`;
  - wraps notes and transcript in tagged blocks labelled as untrusted source material, the same posture as the notes prompts;
  - includes the conversation history up to a 20-message window;
  - instructs the model to:
    - answer only from the meeting and say so when the meeting didn't cover something;
    - cite moments as `[mm:ss]`;
    - reply as JSON `{"reply": "...", "notes": null | {summary, decisions, follow_ups, open_questions}}`;
    - set `notes` only when the user asked to change the notes and `allow_edits` is true;
  - uses a literal example of the shape, as the notes prompt does.
- `parse_chat_reply(raw) -> ChatReply { reply: String, proposal: Option<NotesProposal> }`. It reuses `strip_markdown_fence` and `first_json_object`. If the reply can't be parsed, the raw text becomes `reply` with no proposal (spec: "a model that ignores the format"). A proposal identical to the current notes is dropped.

**Alternative considered:** two calls, one to classify the message as question vs edit and one to answer. That doubles cost and latency. The model already knows whether it was asked for an edit, and the `allow_edits` gate in code (not the model) enforces the recording rule.

`kea_features::meeting::run_chat_turn(engines, bindings, meetings, conversations, meeting_id, message, usage)`:
- loads the detail;
- sets `allow_edits = status != Recording`;
- calls `meetings_llm`, then `complete`;
- records usage (same `record_meeting_usage`);
- appends the user message and the assistant message (with `meta` when there is a proposal);
- returns the assistant message and `truncated`.

A provider failure is returned as an error and stored as nothing, so a retry resends cleanly.

**Concurrency:** one chat turn in flight per meeting. The UI disables Send while one is pending. The command also refuses a second turn for the same meeting via a per-meeting in-flight set.

### 5. Citations are parsed in the UI

The `AskAiTab` renderer splits the reply text on `\[(\d{1,3}):([0-5]\d)\]`. Each match within the meeting's duration becomes a button that calls the pane's `jumpTo(ms)`: switch to the Transcript tab, find the segment whose `[start, end)` contains the time (or the nearest start before it), scroll it into view, and add a highlight that fades.

Out-of-range times stay as text (spec).

Reply text is rendered as plain text with line breaks, not HTML or Markdown, so model output can't inject markup.

### 6. The recording rule is enforced in the backend

These all refuse while `meetings.status = 'recording'`:
- `run_chat_turn` sends `allow_edits = false` and discards any proposal the model returns anyway;
- `apply_notes_proposal`;
- `edit_notes_section`;
- `undo_notes_edit`.

The UI also hides the controls and shows "Editing is available when the meeting ends". Enforcing it in the backend is what makes the guarantee hold even if a stale UI sends the command right as recording starts.

### 7. Commands

| command | purpose |
|---|---|
| `search_meetings(query, limit)` | list rows matching title or notes |
| `get_meeting_chat(meeting_id)` | messages with `meta` |
| `send_meeting_chat(meeting_id, message)` | runs one turn, returns the assistant message and `truncated` |
| `clear_meeting_chat(meeting_id)` | deletes the conversation |
| `apply_notes_proposal(meeting_id, message_id)` | applies, then marks the message `applied` |
| `discard_notes_proposal(meeting_id, message_id)` | marks the message `discarded` |
| `edit_notes_section(meeting_id, section, text)` | a direct edit, versioned |
| `undo_notes_edit(meeting_id)` | pops one version, returns the restored notes |

`MeetingDetail` gains `notes.edited: bool` and `can_undo: bool`.

**Applying is idempotent:** applying a message that is not `pending` is refused, so a double click doesn't push two versions.

## Risks / Trade-offs

- **[Small local models won't follow the JSON shape]** → The fallback shows the prose as an answer. Edits then simply aren't offered, which is the safe direction.
- **[Notes edited after export look different from the export]** → That is expected. The edited mark tells the user, and the export always reads the current notes.
- **[Long chats grow the prompt]** → The history window is capped at 20 messages, and the transcript cap is unchanged.
- **[A large UI refactor regresses an existing behaviour]** → Settings JSX is moved verbatim. Existing `MeetingsPage` and `MeetingDetail` tests are kept and retargeted, not rewritten. A task inventories every current detail action and checks that each is reachable (spec: "Nothing lost").
- **[The migration touches tables the rewrite History uses]** → Both changes are additive: a nullable column and a new table. The History filter has its own regression test.

## Migration Plan

- Migration `0009` is additive. Existing rows get `meeting_id = NULL`, `meta = NULL` and `edited = 0`, which mean "rewrite conversation", "no proposal" and "generated", respectively.
- Existing migration files are not edited (sqlx checksums).
- Rollback by downgrade is unsupported, as for every migration in this repo.
