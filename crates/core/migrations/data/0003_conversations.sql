CREATE TABLE conversations (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    action_id    INTEGER REFERENCES actions(id) ON DELETE SET NULL,
    feature_id   TEXT NOT NULL,
    engine_id    TEXT NOT NULL,
    model        TEXT,
    provider_ref TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL,
    content         TEXT NOT NULL,
    token_count     INTEGER,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_conversations_created ON conversations (created_at DESC);
CREATE INDEX idx_messages_conversation ON messages (conversation_id, id);
