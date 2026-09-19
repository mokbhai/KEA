-- Per-app profiles: one row per "when I am typing into X, do Y".
--
-- One match pair per row, deliberately not an app_profile_matches join table.
-- A profile that needs two bundle ids (Slack and its helper process) is two
-- rows; that duplication is cheaper than a second repo, a second migration and
-- a second editing surface. The UI's mental model is one row per app, so the
-- schema is shaped like the UI.
CREATE TABLE app_profiles (
    id               TEXT PRIMARY KEY NOT NULL,
    name             TEXT NOT NULL,
    enabled          INTEGER NOT NULL DEFAULT 1,
    priority         INTEGER NOT NULL DEFAULT 0,

    -- Match keys. NULL means "any". Both NULL = the catch-all profile.
    -- app_name and window_title are deliberately NOT match keys: they are
    -- localized, volatile display strings that differ per machine.
    match_bundle_id  TEXT,          -- exact, compared case-insensitively
    match_url_glob   TEXT,          -- host[/path] glob, e.g. "*.slack.com/*"

    -- Overrides. NULL means "inherit the global setting"; that is why every
    -- column is nullable and why post_process is a nullable INTEGER rather
    -- than NOT NULL DEFAULT 0 — it is a tri-state (force on / force off /
    -- inherit), and collapsing it to two states would silently turn LLM
    -- cleanup off for every app that never opted in.
    rewrite_mode     TEXT,          -- RewriteMode::as_str()
    preset_id        TEXT REFERENCES rewrite_presets(id) ON DELETE SET NULL,
    llm_engine_id    TEXT,
    llm_model        TEXT,
    llm_provider_ref TEXT,
    post_process     INTEGER,       -- 0 | 1 | NULL
    insertion_mode   TEXT,          -- 'ax' | 'paste' | NULL

    created_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The preset_id foreign key is load-bearing, not decorative: open_pool sets
-- .foreign_keys(true), so deleting a preset nulls the reference here instead
-- of leaving a profile pointing at an id that build_llm_request would later
-- fail on with KeaError::NotFound.
CREATE INDEX idx_app_profiles_lookup
    ON app_profiles (enabled, priority DESC, match_bundle_id);
