# Tasks

## 1. Storage

- [x] 1.1 Add migration `crates/core/migrations/data/0009_meeting_workspace.sql` exactly as design.md decision 3; verify `cargo test -p kea-core` migration tests pass and a test that deleting a meeting cascades to its conversation and notes versions
- [x] 1.2 Exclude meeting chats from the rewrite History query (`meeting_id IS NULL`); verify a regression test that a meeting conversation does not appear in History
- [x] 1.3 `MeetingRepo::search(query, limit)` over title and notes sections with `%`/`_` escaping; verify tests for a title hit, a summary-only hit, a case-insensitive hit and a literal `%` query
- [x] 1.4 Notes versioning: `apply_user_notes(meeting_id, notes)` (push the current notes onto the versions table, write the new notes with `edited = 1`, cap at 50) and `undo_notes(meeting_id)` (pop and restore including `edited`); `MeetingDetail` gains `notes.edited` and `can_undo`; verify tests for apply→undo→generated clears edited, the 50-version cap, and undo with an empty stack
- [x] 1.5 Meeting conversations: lazily create a conversation per meeting, append messages with `meta`, list them, clear them, and set a proposal's state; verify store tests including clear and state transitions (pending→applied, pending→discarded, refuse applied→applied)

## 2. Chat core (pure)

- [x] 2.1 `kea_core::meetings::chat::build_chat_request` per design.md decision 4 (untrusted blocks, 20-message window, `allow_edits` wording, literal JSON example, `truncated`); verify tests that the transcript and notes are in tagged blocks, that edits are not invited when `allow_edits` is false, and that `truncated` is set past the cap
- [x] 2.2 `parse_chat_reply` with the prose fallback and identical-proposal drop; verify tests for valid JSON, fenced JSON, prose, a proposal equal to the current notes, and a proposal with missing sections (missing means unchanged)

## 3. Feature and commands

- [ ] 3.1 `run_chat_turn` in `crates/features/src/meeting.rs` (meetings binding, usage recorded, messages appended, proposals discarded while recording, nothing stored on provider failure, one turn in flight per meeting); verify feature tests with a scripted LLM for a question, a proposal on a stopped meeting, a proposal dropped while recording, a failure, and a concurrent second turn refused
- [ ] 3.2 Tauri commands from design.md decision 7, with the backend recording refusal on apply/edit/undo; verify `cargo test -p kea-app` tests for refusal while recording and idempotent apply
- [ ] 3.3 Expose the commands and types in `ui/src/api.ts`; verify the ui typecheck and api tests

## 4. Workspace UI

- [ ] 4.1 Inventory every action and setting on the current Meetings page and `MeetingDetail` into a checklist in this change's folder (`ui-inventory.md`); verify each later UI task ticks its items and none is left unticked at the end
- [ ] 4.2 Move the settings into `MeetingSettingsPanel` (slide-over; ⚙, close, Escape and outside-click; focus returns to ⚙; unsaved state kept); verify retargeted `MeetingsPage` settings tests pass and new tests for Escape and focus return
- [ ] 4.3 `MeetingList` with grouping (local time), the pinned recording row, row details, the debounced search via `search_meetings`, and the no-match state; verify component tests for grouping across midnight, the pinned row and search
- [ ] 4.4 Page shell: grid layout, default selection (recording, else newest), selection kept across page switches in the app session, empty state, and the one-pane narrow layout with Back; verify tests for each default-selection case and the narrow layout
- [ ] 4.5 `MeetingPane` with Summary/Transcript/Ask AI tabs and per-tab scroll memory; split `MeetingDetail` into `SummaryTab` and `TranscriptTab` keeping rename, speaker rename/confirm, action items, copy, export and delete; verify retargeted `MeetingDetail` tests and the inventory
- [ ] 4.6 Live view in the pane (live notes and transcript from events, follow-unless-scrolled-up, "Generating notes…" on stop, then refetch); remove "Try it" and move Record/Stop to the header; verify tests driving fake segment and notes events through a stop

## 5. Ask AI and editing UI

- [ ] 5.1 `AskAiTab`: history load, send with a pending bubble and Send disabled, plain-text rendering, a truncated-transcript notice, a no-LLM-bound state, Clear chat, and quick prompts (Translate asks for a language); verify component tests for each
- [ ] 5.2 Citations: parse `[mm:ss]`, link in-range times, `jumpTo` switches to Transcript, scrolls and highlights the containing segment; verify a test with a fake transcript that a click lands on the right segment and that an out-of-range time is text
- [ ] 5.3 Proposal card: before/after per changed section, Apply and Discard, the state shown in history, Apply disabled once applied; verify tests that the notes change only on Apply and that a double click applies once
- [ ] 5.4 Direct section edit (Edit → textarea → Save/Cancel), the edited badge, and Undo on the Summary tab; all hidden with an explanation while recording; verify tests for edit→undo and for the hidden state during recording

## 6. Verification

- [ ] 6.1 Run `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt --check`, and the UI test and typecheck scripts; all pass, and `ui-inventory.md` is fully ticked
- [ ] 6.2 In the running app: open Meetings with several past meetings and confirm the newest summary shows without scrolling; search by a word from a summary; record a short meeting and watch it live in the pane; ask a question and follow a citation; apply a "shorter" proposal, undo it, and edit a section by hand
