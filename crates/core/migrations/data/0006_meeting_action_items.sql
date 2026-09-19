-- Action items as rows rather than a paragraph, so one can be ticked off,
-- counted or exported. `meeting_notes.action_items` stays populated as a
-- derived view of these rows: meetings recorded before this table still
-- render, and nothing that reads the column has to learn about the table.
--
-- `due_hint` is stored exactly as the model said it ("by Friday"), never
-- resolved to a date: resolving one means guessing the meeting's timezone and
-- which Friday was meant, and a wrong due date is worse than a quoted phrase.
CREATE TABLE meeting_action_items (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id    TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    text          TEXT NOT NULL,
    owner         TEXT,
    due_hint      TEXT,
    source_seq    INTEGER,
    status        TEXT NOT NULL DEFAULT 'open',
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_meeting_action_items_meeting ON meeting_action_items (meeting_id, id);
