-- Speaker attribution for meeting transcripts.
--
-- `speaker_key` is NULL on every row written before this migration, and NULL
-- reads as "unknown speaker" — which renders exactly as a pre-diarization
-- transcript did. Nothing is backfilled: the audio is gone, so there is no
-- honest label to assign retroactively.
ALTER TABLE meeting_segments ADD COLUMN speaker_key TEXT;

-- One row per side of a meeting, so the user can rename "You" and "Others"
-- without the names leaking into other meetings. `source` records who chose
-- the name: 'channel' is the default this feature wrote, 'user' is a name a
-- human typed, and a re-run of attribution must never overwrite the latter.
CREATE TABLE meeting_speakers (
    meeting_id   TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    speaker_key  TEXT NOT NULL,
    display_name TEXT NOT NULL,
    source       TEXT NOT NULL,
    PRIMARY KEY (meeting_id, speaker_key)
);
