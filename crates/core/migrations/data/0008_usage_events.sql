-- One row per LLM call, with the token counts the provider reported.
--
-- Not columns on `actions`: one action can make several calls (a meeting stop
-- runs notes, a repair round trip and a title), so totals on the action row
-- would have to be summed at write time by whoever happened to be last.
-- Not `messages.token_count` either — that column is filled too, but only when
-- the user has content storage on, and someone who does not want their words
-- kept still wants to know what they spent.
--
-- Both token columns are NULL when the provider reported no usage block, which
-- llama.cpp and several compatible servers do not. NULL is not zero: the view
-- counts those calls separately and says how many there were rather than
-- folding an unknown into a total.
CREATE TABLE usage_events (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    -- NULL for a call made outside a ledger run (an interim notes pass owns
    -- no action row of its own).
    action_id         INTEGER REFERENCES actions(id) ON DELETE SET NULL,
    feature_id        TEXT NOT NULL,
    engine_id         TEXT NOT NULL,
    model             TEXT,
    provider_ref      TEXT,
    prompt_tokens     INTEGER,
    completion_tokens INTEGER,
    created_at        TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Every query the view makes is "since <date>, grouped"; the index carries the
-- range scan.
CREATE INDEX idx_usage_events_created ON usage_events (created_at DESC);
