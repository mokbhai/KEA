CREATE TABLE vocabulary (
  id          TEXT PRIMARY KEY,
  term        TEXT NOT NULL,          -- the correct spelling, e.g. "KittyClaw"
  sounds_like TEXT,                   -- optional comma-separated misrecognitions
  enabled     INTEGER NOT NULL DEFAULT 1,
  created_at  TEXT NOT NULL
);
CREATE UNIQUE INDEX idx_vocabulary_term ON vocabulary(term COLLATE NOCASE);
