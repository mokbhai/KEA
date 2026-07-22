CREATE TABLE rewrite_presets (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    instruction TEXT NOT NULL,
    sort_order  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE rewrite_prompt_overrides (
    mode        TEXT PRIMARY KEY NOT NULL,
    prompt      TEXT NOT NULL
);

CREATE TABLE hotkey_bindings (
    feature_id  TEXT NOT NULL,
    command     TEXT NOT NULL,
    accelerator TEXT NOT NULL,
    PRIMARY KEY (feature_id, command)
);
