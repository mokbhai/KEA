-- Styles replace presets.
--
-- Before: a built-in "style" (rewrite.active_mode) and, separately, a saved
-- "preset" (rewrite_presets + rewrite.active_preset_id) that overrode the
-- style. After: one concept. A custom style is a name + an instruction, and the
-- active style is the single value rewrite.active_mode, which now names either
-- a built-in mode or a custom style id. Per-preset LLM overrides are dropped: a
-- custom style runs on the Rewrite AI like every built-in style.

CREATE TABLE rewrite_styles (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    instruction TEXT NOT NULL,
    sort_order  INTEGER NOT NULL DEFAULT 0
);

-- Every preset becomes a custom style with the same id, name and instruction,
-- except the junk "None" preset: an instruction that is exactly "none"
-- (any case, surrounding whitespace ignored) was how a user once switched the
-- preset off, and would otherwise become a style that rewrites text into the
-- word "None".
INSERT INTO rewrite_styles (id, name, instruction, sort_order)
SELECT id, name, instruction, sort_order
FROM rewrite_presets
WHERE lower(trim(instruction, ' ' || char(9) || char(10) || char(13))) <> 'none';

-- The active preset used to win over the active mode. If it was a preset that
-- survived above, that style becomes the active style; if it was a skipped or
-- missing one, rewrite.active_mode is left exactly as it was. Settings values
-- are JSON, hence json_extract; json_valid keeps a hand-edited value from
-- failing the whole migration.
INSERT INTO settings (key, value)
SELECT 'rewrite.active_mode', json_quote(s.id)
FROM settings p
JOIN rewrite_styles s
  ON s.id = CASE WHEN json_valid(p.value) THEN json_extract(p.value, '$') END
WHERE p.key = 'rewrite.active_preset_id'
ON CONFLICT(key) DO UPDATE SET value = excluded.value;

DELETE FROM settings WHERE key = 'rewrite.active_preset_id';

-- app_profiles carried the same split (rewrite_mode + preset_id, the latter a
-- foreign key into rewrite_presets). SQLite cannot drop a column that is part
-- of a foreign key, so the table is rebuilt with one rewrite_style column that
-- names a built-in or a custom style. A profile's preset won over its mode, so
-- it still does when it migrated; otherwise the mode carries over unchanged.
CREATE TABLE app_profiles_new (
    id               TEXT PRIMARY KEY NOT NULL,
    name             TEXT NOT NULL,
    enabled          INTEGER NOT NULL DEFAULT 1,
    priority         INTEGER NOT NULL DEFAULT 0,
    match_bundle_id  TEXT,
    match_url_glob   TEXT,
    rewrite_style    TEXT,          -- RewriteMode::as_str() or a rewrite_styles.id
    llm_engine_id    TEXT,
    llm_model        TEXT,
    llm_provider_ref TEXT,
    post_process     INTEGER,
    insertion_mode   TEXT,
    created_at       TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO app_profiles_new (
    id, name, enabled, priority, match_bundle_id, match_url_glob, rewrite_style,
    llm_engine_id, llm_model, llm_provider_ref, post_process, insertion_mode, created_at
)
SELECT
    p.id, p.name, p.enabled, p.priority, p.match_bundle_id, p.match_url_glob,
    COALESCE((SELECT s.id FROM rewrite_styles s WHERE s.id = p.preset_id), p.rewrite_mode),
    p.llm_engine_id, p.llm_model, p.llm_provider_ref, p.post_process, p.insertion_mode,
    p.created_at
FROM app_profiles p;

DROP TABLE app_profiles;
ALTER TABLE app_profiles_new RENAME TO app_profiles;
CREATE INDEX idx_app_profiles_lookup
    ON app_profiles (enabled, priority DESC, match_bundle_id);

DROP TABLE rewrite_presets;
