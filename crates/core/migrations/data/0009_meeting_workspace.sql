-- Storage for the meeting workspace: a chat per meeting, proposal state on
-- chat messages, and an undo stack for the notes.
--
-- Additive throughout. Existing conversation rows get meeting_id = NULL (a
-- rewrite conversation), existing messages get meta = NULL (no proposal) and
-- existing notes get edited = 0 (generated, unchanged by the user).

-- A meeting's chat is a conversation tagged with its meeting. Reusing the
-- conversations and messages tables keeps one message model, and the cascade
-- is what deletes a chat with its meeting.
ALTER TABLE conversations ADD COLUMN meeting_id TEXT REFERENCES meetings(id) ON DELETE CASCADE;
CREATE INDEX idx_conversations_meeting ON conversations (meeting_id, id);

-- JSON on an assistant message that proposed a notes edit: the proposal and
-- its state. One column, because only the UI reads it back and it has a
-- single writer.
ALTER TABLE messages ADD COLUMN meta TEXT;

-- Set by a user change (an applied proposal or a direct edit), cleared by
-- generation. The UI shows it as the "edited" mark.
ALTER TABLE meeting_notes ADD COLUMN edited INTEGER NOT NULL DEFAULT 0;

-- The undo stack. Each row is the notes as they were before a user change,
-- so undo restores the newest row and deletes it.
CREATE TABLE meeting_notes_versions (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id  TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    notes_json  TEXT NOT NULL,
    edited      INTEGER NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX idx_notes_versions_meeting ON meeting_notes_versions (meeting_id, id);
