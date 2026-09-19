-- What a model costs, as the *user* typed it.
--
-- There is deliberately no seeded price list. A table of 2026 prices compiled
-- into the binary reads as authoritative and is wrong by the following year,
-- and a cost view that quietly reports last year's prices is worse than one
-- that reports tokens alone. So: no rate, no money — the usage view shows
-- tokens for every call and a price only for the models someone has entered a
-- rate for, next to the date they entered it.
--
-- `provider_key` is the provider the call was billed to: the binding's
-- `provider_ref` when it has one, otherwise the engine id (see
-- `store::rates::provider_key`). Two OpenAI-compatible providers serving the
-- same model name charge different prices, and the engine id alone cannot
-- tell them apart.
CREATE TABLE llm_rates (
    provider_key    TEXT NOT NULL,
    model           TEXT NOT NULL,
    -- Per million tokens, in `currency`. Per million rather than per token
    -- because that is the unit every provider publishes, and a rate typed in
    -- the unit it was read in cannot be mistyped by six orders of magnitude.
    input_per_mtok  REAL NOT NULL,
    output_per_mtok REAL NOT NULL,
    currency        TEXT NOT NULL DEFAULT 'USD',
    -- When the user last touched this rate. Shown in the view, because a rate
    -- is only as good as its date and only the user knows how stale it is.
    updated_at      TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (provider_key, model)
);
