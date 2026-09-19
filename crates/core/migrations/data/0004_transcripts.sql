-- File transcription: a dropped audio/video file and its timed cues.
--
-- Its own tables rather than `conversations` (a role-tagged message log with
-- nowhere to put a timestamp) and rather than `meetings` (whose
-- started_at/ended_at/capture_mode/status describe a live capture session — a
-- dropped podcast has none of those and must not appear in the Meetings list).
-- Shaped after `meeting_segments`, which is the right precedent.
CREATE TABLE transcripts (
    id              TEXT PRIMARY KEY NOT NULL,
    source_path     TEXT NOT NULL,
    source_filename TEXT NOT NULL,
    duration_ms     INTEGER NOT NULL DEFAULT 0,
    stt_engine_id   TEXT,
    model           TEXT,
    language        TEXT,
    status          TEXT NOT NULL,
    error           TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE transcript_segments (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    transcript_id TEXT NOT NULL REFERENCES transcripts(id) ON DELETE CASCADE,
    sequence      INTEGER NOT NULL,
    start_ms      INTEGER NOT NULL,
    end_ms        INTEGER NOT NULL,
    text          TEXT NOT NULL,
    -- Diarization label, NULL when nothing was run. A new table, so this is a
    -- column from the start rather than an ALTER in a later migration.
    speaker_key   TEXT,
    UNIQUE (transcript_id, sequence)
);

CREATE INDEX idx_transcripts_created ON transcripts (created_at DESC);
CREATE INDEX idx_transcript_segments_transcript
    ON transcript_segments (transcript_id, sequence);
