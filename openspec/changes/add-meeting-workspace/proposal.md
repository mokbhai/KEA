# Proposal

## Why

The Meetings page is one long column, in this order: about 150 lines of behaviour settings, then "Try it", live notes, live transcript, and finally the past-meetings list and the selected meeting at the very bottom. Reading the summary of yesterday's meeting, the most common thing anyone opens the page for, means scrolling past every setting, finding the list, clicking, and scrolling again.

Once notes exist there is also no way to work with them. You can't ask a question about the meeting ("what did Dan say about CI/CD?"), and you can't reshape the summary for a purpose ("make it shorter", "rewrite this for my manager"). The notes can't be edited at all, even by hand.

## What Changes

- **A two-pane meeting workspace** replaces the long column, inside the existing Meetings sidebar entry. No new sidebar item.
  - **Header:** the title, the Record/Stop button, and a ⚙ button.
  - **Left pane:** a searchable list of meetings, grouped Today / Yesterday / This week / Earlier, with a recording meeting pinned on top.
  - **Right pane:** the selected meeting with three tabs: **Summary** (notes and action items), **Transcript**, and **Ask AI**.
  - **On open,** the page selects the meeting being recorded, or else the most recent one, so its summary is visible without scrolling.
  - **While recording,** the right pane shows the live notes and live transcript. The "Try it" section is removed.
- **Settings move into a slide-over panel** opened by ⚙. Every existing meeting setting keeps its behaviour, just in a different place.
- **"Ask AI" chat for each meeting.** Its context is the meeting's notes, action items and full transcript, with speaker names.
  - Answers cite timestamps, and clicking one jumps to that line in the Transcript tab.
  - The conversation is saved with the meeting.
  - It works during a live meeting for questions about what has been said so far.
- **Notes edits via chat.** When a request asks to change the notes, the reply carries a *proposed* version, shown section by section with **Apply** and **Discard**. Nothing changes until Apply.
  - An applied edit can be undone.
  - Notes edited by chat or by hand are marked as edited.
  - Edit proposals are offered only after the meeting has stopped, because interim and final passes rewrite the notes while it records.
- **Direct editing of the notes:** each notes section on the Summary tab gets an Edit action. It shares the same version history and undo.
- **Quick prompts** above the chat input: Shorter, Email-ready, Only decisions, Translate.

## Capabilities

### New Capabilities
- `meetings/meeting-workspace`: the list-and-detail layout, the default selection, search and grouping, the tabs, the live view, the settings slide-over, and the narrow-window behaviour.
- `meetings/meeting-chat`: the per-meeting chat and its context, citations, persistence, notes-edit proposals with apply/discard/undo, direct editing, and the rules around a recording meeting.

### Modified Capabilities
<!-- None: openspec/specs/ has no existing meeting specs. -->

## Impact

- **`ui/src/pages/MeetingsPage.tsx`** is split into components: a meeting list, a detail pane with tabs, a live pane, a settings slide-over, and an Ask AI tab. `MeetingDetail.tsx` becomes the Summary and Transcript tab bodies.
- **`crates/core`:**
  - migration `0009` adds `conversations.meeting_id`, so the existing conversations/messages tables hold meeting chats;
  - it adds `meeting_notes.edited`, plus a `meeting_notes_versions` table for undo;
  - repo methods for search, notes versions and meeting conversations;
  - a chat prompt builder and a tolerant reply parser in `meetings/`.
- **`crates/features/src/meeting.rs`:** a chat turn over the meetings LLM binding, recorded in the usage ledger like the other meeting calls.
- **`src-tauri`:** commands for search, chat send/history, apply/discard/undo, and direct section edits.
- **Cost:** one LLM call per chat message, against the provider already bound for meetings. No new data leaves the machine: the same transcript already goes to that provider for notes.
- **Out of scope:**
  - chat across several meetings;
  - retrieval over transcripts longer than the existing 80,000-character cap (they are truncated, and the chat says so);
  - voice input in the chat;
  - sharing or exporting a chat.
