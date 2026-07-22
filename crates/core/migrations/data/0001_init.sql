CREATE TABLE actions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    feature_id   TEXT NOT NULL,
    command      TEXT NOT NULL,
    engine_id    TEXT NOT NULL,
    model        TEXT,
    provider_ref TEXT,
    started_at   TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at  TEXT,
    status       TEXT NOT NULL DEFAULT 'started',
    error        TEXT
);
