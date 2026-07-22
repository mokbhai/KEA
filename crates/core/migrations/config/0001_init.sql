CREATE TABLE settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL          -- JSON-encoded value
);

CREATE TABLE bindings (
    feature_id   TEXT NOT NULL,
    slot         TEXT NOT NULL,
    engine_id    TEXT NOT NULL,
    model        TEXT,
    provider_ref TEXT,
    PRIMARY KEY (feature_id, slot)
);
