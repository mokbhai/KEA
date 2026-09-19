//! What a model costs, as the user typed it.
//!
//! # Why there is no built-in price list
//!
//! Prices change, and a table compiled into the binary cannot. A KEA built in
//! 2026 would still be quoting 2026 prices in 2028, with the same confident
//! formatting it uses for the token counts it actually measured — and the user
//! has no way to tell the measured number from the stale one. Tokens are a
//! fact KEA observed; money is a claim about the world, and KEA does not have
//! one unless somebody gives it one.
//!
//! So: the rate table starts empty, the usage view shows tokens for
//! everything, and spend appears only beside a model someone entered a rate
//! for — next to the date they entered it, so a rate going stale is visible
//! rather than silent.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::KeaError;

/// The provider a call is billed to.
///
/// The binding's `provider_ref` when it has one, otherwise the engine id. Two
/// OpenAI-compatible endpoints can serve the same model name at different
/// prices — that is the whole reason custom providers exist — so the engine id
/// alone is not a billing identity.
pub fn provider_key(engine_id: &str, provider_ref: Option<&str>) -> String {
    provider_ref.unwrap_or(engine_id).to_string()
}

/// One user-entered price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct LlmRate {
    /// See [`provider_key`].
    pub provider_key: String,
    pub model: String,
    /// Per million tokens, in [`LlmRate::currency`] — the unit providers
    /// publish, so a rate cannot be mistyped by six orders of magnitude.
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub currency: String,
    /// When the user last touched this rate. Shown wherever the money is.
    pub updated_at: String,
}

impl LlmRate {
    /// What `prompt` + `completion` tokens cost at this rate.
    ///
    /// Plain arithmetic, deliberately: no minimum charge, no cached-input
    /// discount, no batch pricing. Every provider prices those differently and
    /// KEA is not told which applied, so this is an estimate from the two
    /// numbers the user gave — which is exactly what the view calls it.
    pub fn cost_of(&self, prompt: i64, completion: i64) -> f64 {
        (prompt as f64 * self.input_per_mtok + completion as f64 * self.output_per_mtok)
            / 1_000_000.0
    }
}

/// One usage total with whatever the rate table can say about its cost.
///
/// The three money fields travel together and are all `None` together: a cost
/// with no currency is meaningless, and a cost with no date is a claim with no
/// provenance. Nothing downstream has to decide what a partial row means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSpend {
    pub feature_id: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
    /// The key a rate for this row would be filed under — shown in the view so
    /// "there is no rate for this" comes with the two words needed to add one.
    pub provider_key: String,
    pub calls: i64,
    pub unreported_calls: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    /// `None` when the model is unknown to the rate table, when no rate has
    /// been entered for it, or when *any* of the calls reported no usage — in
    /// that last case the tokens are a floor, so a cost computed from them
    /// would be a floor presented as a total. See [`UsageSpend::cost`].
    pub cost: Option<f64>,
    pub currency: Option<String>,
    pub rate_updated_at: Option<String>,
}

/// Attaches costs to totals, and only where there is honest ground for one.
///
/// Two reasons a row comes back with no money, both deliberate:
///
/// * **no rate** — KEA ships none, so this is the ordinary state until the
///   user enters one. The row still shows its tokens.
/// * **an unreported call in the group** — some provider in this group sent no
///   usage block, so the token sums are a floor. Pricing a floor and printing
///   it next to rows that are complete invites exactly the comparison it
///   cannot support.
pub fn priced(totals: Vec<crate::store::usage::UsageTotal>, rates: &[LlmRate]) -> Vec<UsageSpend> {
    totals
        .into_iter()
        .map(|t| {
            let key = provider_key(&t.engine_id, t.provider_ref.as_deref());
            let rate = t.model.as_deref().and_then(|model| {
                rates
                    .iter()
                    .find(|r| r.provider_key == key && r.model == model)
            });
            let rate = rate.filter(|_| t.unreported_calls == 0);
            UsageSpend {
                provider_key: key,
                cost: rate.map(|r| r.cost_of(t.prompt_tokens, t.completion_tokens)),
                currency: rate.map(|r| r.currency.clone()),
                rate_updated_at: rate.map(|r| r.updated_at.clone()),
                feature_id: t.feature_id,
                engine_id: t.engine_id,
                model: t.model,
                provider_ref: t.provider_ref,
                calls: t.calls,
                unreported_calls: t.unreported_calls,
                prompt_tokens: t.prompt_tokens,
                completion_tokens: t.completion_tokens,
            }
        })
        .collect()
}

pub struct RateRepo {
    pool: SqlitePool,
}

impl RateRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn list(&self) -> Result<Vec<LlmRate>, KeaError> {
        let rows = sqlx::query_as::<_, LlmRate>(
            "SELECT provider_key, model, input_per_mtok, output_per_mtok, currency, updated_at
             FROM llm_rates ORDER BY provider_key, model",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Stores a rate and stamps it with now.
    ///
    /// `updated_at` is written here rather than taken from the caller because
    /// it is the one field whose whole job is to be trustworthy: a date the
    /// UI could send is a date a round trip could preserve from the row it
    /// just edited, and a stale rate would keep looking fresh.
    pub async fn upsert(&self, rate: &LlmRate) -> Result<(), KeaError> {
        if rate.provider_key.trim().is_empty() || rate.model.trim().is_empty() {
            return Err(KeaError::Other(
                "a rate needs both a provider and a model".into(),
            ));
        }
        // A negative price is a typo, and one that would read as a credit in
        // the totals. Zero is allowed: a self-hosted model really is free.
        if rate.input_per_mtok < 0.0 || rate.output_per_mtok < 0.0 {
            return Err(KeaError::Other("a rate cannot be negative".into()));
        }
        sqlx::query(
            "INSERT INTO llm_rates(provider_key, model, input_per_mtok, output_per_mtok,
                                   currency, updated_at)
             VALUES(?, ?, ?, ?, ?, datetime('now'))
             ON CONFLICT(provider_key, model) DO UPDATE SET
               input_per_mtok = excluded.input_per_mtok,
               output_per_mtok = excluded.output_per_mtok,
               currency = excluded.currency,
               updated_at = excluded.updated_at",
        )
        .bind(rate.provider_key.trim())
        .bind(rate.model.trim())
        .bind(rate.input_per_mtok)
        .bind(rate.output_per_mtok)
        .bind(rate.currency.trim())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete(&self, provider_key: &str, model: &str) -> Result<(), KeaError> {
        sqlx::query("DELETE FROM llm_rates WHERE provider_key = ? AND model = ?")
            .bind(provider_key)
            .bind(model)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    async fn repo() -> RateRepo {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        RateRepo::new(pool)
    }

    fn rate(provider: &str, model: &str) -> LlmRate {
        LlmRate {
            provider_key: provider.into(),
            model: model.into(),
            input_per_mtok: 0.15,
            output_per_mtok: 0.60,
            currency: "USD".into(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn the_provider_ref_is_the_billing_identity_when_there_is_one() {
        assert_eq!(provider_key("openai-compatible", Some("work")), "work");
        // No ref: the engine is all we know, and the engine is who is billing.
        assert_eq!(provider_key("openai", None), "openai");
    }

    #[test]
    fn cost_is_per_million_tokens() {
        let r = rate("openai", "gpt-4o-mini");
        // 1M in at 0.15 and 1M out at 0.60.
        assert!((r.cost_of(1_000_000, 1_000_000) - 0.75).abs() < 1e-9);
        assert_eq!(r.cost_of(0, 0), 0.0);
    }

    fn total(model: Option<&str>, unreported: i64) -> crate::store::usage::UsageTotal {
        crate::store::usage::UsageTotal {
            feature_id: "rewrite".into(),
            engine_id: "openai-compatible".into(),
            model: model.map(str::to_string),
            provider_ref: Some("work".into()),
            calls: 2,
            unreported_calls: unreported,
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
        }
    }

    #[test]
    fn a_priced_row_carries_the_rate_and_its_date() {
        let mut r = rate("work", "gpt-4o-mini");
        r.updated_at = "2026-09-01 00:00:00".into();
        let out = priced(vec![total(Some("gpt-4o-mini"), 0)], &[r]);
        assert!((out[0].cost.unwrap() - 0.75).abs() < 1e-9);
        assert_eq!(out[0].currency.as_deref(), Some("USD"));
        assert_eq!(
            out[0].rate_updated_at.as_deref(),
            Some("2026-09-01 00:00:00")
        );
        // The key is shown so the user knows what a rate would be filed under.
        assert_eq!(out[0].provider_key, "work");
    }

    #[test]
    fn no_rate_means_tokens_only() {
        let out = priced(vec![total(Some("gpt-4o-mini"), 0)], &[]);
        assert_eq!(out[0].cost, None);
        assert_eq!(out[0].currency, None);
        assert_eq!(
            out[0].prompt_tokens, 1_000_000,
            "the tokens are still there"
        );
    }

    #[test]
    fn a_rate_filed_under_another_provider_does_not_apply() {
        // Two endpoints serving "gpt-4o-mini" at different prices is exactly
        // why the key includes the provider.
        let out = priced(
            vec![total(Some("gpt-4o-mini"), 0)],
            &[rate("openai", "gpt-4o-mini")],
        );
        assert_eq!(out[0].cost, None);
    }

    #[test]
    fn an_incomplete_group_is_not_priced() {
        // One call in this group reported nothing, so the token sums are a
        // floor. A price on a floor reads as a total.
        let out = priced(
            vec![total(Some("gpt-4o-mini"), 1)],
            &[rate("work", "gpt-4o-mini")],
        );
        assert_eq!(out[0].cost, None);
        assert_eq!(out[0].unreported_calls, 1);
    }

    #[test]
    fn a_call_with_no_model_is_never_priced() {
        // Nothing names what was billed, so nothing can be looked up.
        let out = priced(vec![total(None, 0)], &[rate("work", "gpt-4o-mini")]);
        assert_eq!(out[0].cost, None);
    }

    #[tokio::test]
    async fn the_table_starts_empty() {
        // The feature's central claim: KEA ships no prices, so it can never
        // quote one it did not learn from the user.
        assert!(repo().await.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn upsert_stamps_the_date_and_replaces_in_place() {
        let repo = repo().await;
        repo.upsert(&rate("openai", "gpt-4o-mini")).await.unwrap();
        let stored = repo.list().await.unwrap();
        assert_eq!(stored.len(), 1);
        assert!(!stored[0].updated_at.is_empty(), "stamped by the database");

        let mut cheaper = rate("openai", "gpt-4o-mini");
        cheaper.input_per_mtok = 0.10;
        repo.upsert(&cheaper).await.unwrap();
        let stored = repo.list().await.unwrap();
        assert_eq!(stored.len(), 1, "same key, one row");
        assert_eq!(stored[0].input_per_mtok, 0.10);
    }

    #[tokio::test]
    async fn two_providers_may_price_the_same_model_differently() {
        let repo = repo().await;
        repo.upsert(&rate("openai", "gpt-4o-mini")).await.unwrap();
        let mut resold = rate("work", "gpt-4o-mini");
        resold.input_per_mtok = 1.0;
        repo.upsert(&resold).await.unwrap();
        assert_eq!(repo.list().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_negative_or_nameless_rate_is_refused() {
        let repo = repo().await;
        let mut credit = rate("openai", "gpt-4o-mini");
        credit.output_per_mtok = -1.0;
        assert!(repo.upsert(&credit).await.is_err());

        assert!(repo.upsert(&rate("openai", "  ")).await.is_err());
        assert!(repo.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_removes_one_rate() {
        let repo = repo().await;
        repo.upsert(&rate("openai", "gpt-4o-mini")).await.unwrap();
        repo.delete("openai", "gpt-4o-mini").await.unwrap();
        assert!(repo.list().await.unwrap().is_empty());
    }
}
