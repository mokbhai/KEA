-- A preset may name its own LLM, so one shortcut can use a cheap model for
-- grammar and another an expensive one for a hard rewrite.
--
-- The same three columns as `app_profiles`, on purpose: both are an optional
-- override of the `rewrite/llm` binding, and both are read through
-- `Binding::from_parts`, so there is one rule about what a half-filled
-- override means (an engine with no model is a binding; a model with no engine
-- is nothing) rather than two that can drift apart.
ALTER TABLE rewrite_presets ADD COLUMN llm_engine_id    TEXT;
ALTER TABLE rewrite_presets ADD COLUMN llm_model        TEXT;
ALTER TABLE rewrite_presets ADD COLUMN llm_provider_ref TEXT;
