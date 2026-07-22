CREATE TABLE meetings (
    id              TEXT PRIMARY KEY NOT NULL,
    title           TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    ended_at        TEXT,
    status          TEXT NOT NULL,
    capture_mode    TEXT NOT NULL,
    stt_engine_id   TEXT,
    llm_engine_id   TEXT,
    error           TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE meeting_segments (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id      TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    sequence        INTEGER NOT NULL,
    start_offset_ms INTEGER NOT NULL,
    end_offset_ms   INTEGER NOT NULL,
    text            TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (meeting_id, sequence)
);

CREATE TABLE meeting_notes (
    meeting_id      TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
    summary         TEXT NOT NULL DEFAULT '',
    decisions       TEXT NOT NULL DEFAULT '',
    action_items    TEXT NOT NULL DEFAULT '',
    follow_ups      TEXT NOT NULL DEFAULT '',
    open_questions  TEXT NOT NULL DEFAULT '',
    prompt_version  TEXT NOT NULL,
    engine_id       TEXT,
    model           TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_meetings_started ON meetings (started_at DESC);
CREATE INDEX idx_meeting_segments_meeting ON meeting_segments (meeting_id, sequence);
