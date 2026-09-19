//! The token-usage ledger: one row per LLM call, and the roll-ups the usage
//! view reads.
//!
//! Every number here came from a provider. Nothing in this module estimates a
//! token count, because a number KEA computed itself would disagree with the
//! bill the user is checking it against — every backend tokenizes differently
//! — and a plausible wrong number in a cost view is worse than a blank one.
//! A call whose provider reported no usage block is recorded with both counts
//! NULL and shows up in [`UsageTotal::unreported_calls`], which is how the
//! view says "there were 12 more calls, and nobody told us what they cost".

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::KeaError;

/// One LLM call, as it is about to be written down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewUsageEvent {
    /// The ledger run this call belongs to, when it has one. An interim notes
    /// pass owns no action row, so `None` is an ordinary value here.
    pub action_id: Option<i64>,
    pub feature_id: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
    /// `None` — not `Some(0)` — when the provider reported no usage.
    pub prompt_tokens: Option<i64>,
    pub completion_tokens: Option<i64>,
}

impl NewUsageEvent {
    /// A call with no reported usage, which is the honest default: the caller
    /// fills the counts in only when a provider actually sent them.
    pub fn new(feature_id: impl Into<String>, engine_id: impl Into<String>) -> Self {
        Self {
            action_id: None,
            feature_id: feature_id.into(),
            engine_id: engine_id.into(),
            model: None,
            provider_ref: None,
            prompt_tokens: None,
            completion_tokens: None,
        }
    }
}

/// Everything one (feature, engine, model, provider) spent over a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct UsageTotal {
    pub feature_id: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
    pub calls: i64,
    /// Calls in `calls` whose provider reported nothing. The token sums beside
    /// them are therefore a floor, not a total, and the view says so.
    pub unreported_calls: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

impl UsageTotal {
    pub fn total_tokens(&self) -> i64 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }
}

/// One day's totals, for the "over time" strip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct UsageDay {
    /// `YYYY-MM-DD`, in the database's UTC clock — the same clock every
    /// `created_at` in these tables is stamped from.
    pub day: String,
    pub calls: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
}

/// The two aggregates share this `SELECT` list; only the grouping differs, and
/// a second copy is how the NULL handling ends up different in one of them.
const USAGE_SUMS: &str = "COUNT(*) AS calls,
     SUM(CASE WHEN prompt_tokens IS NULL AND completion_tokens IS NULL THEN 1 ELSE 0 END)
         AS unreported_calls,
     COALESCE(SUM(prompt_tokens), 0) AS prompt_tokens,
     COALESCE(SUM(completion_tokens), 0) AS completion_tokens";

/// `created_at` is stored as `datetime('now')`, so the window is expressed in
/// the same dialect rather than formatted in Rust.
const SINCE: &str = "created_at >= datetime('now', printf('-%d days', ?))";

pub struct UsageRepo {
    pool: SqlitePool,
}

impl UsageRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn record(&self, event: &NewUsageEvent) -> Result<i64, KeaError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO usage_events(action_id, feature_id, engine_id, model, provider_ref,
                                      prompt_tokens, completion_tokens)
             VALUES(?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(event.action_id)
        .bind(&event.feature_id)
        .bind(&event.engine_id)
        .bind(&event.model)
        .bind(&event.provider_ref)
        .bind(event.prompt_tokens)
        .bind(event.completion_tokens)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// Per feature and per provider/model, over the last `days` days.
    pub async fn totals(&self, days: i64) -> Result<Vec<UsageTotal>, KeaError> {
        let rows = sqlx::query_as::<_, UsageTotal>(&format!(
            "SELECT feature_id, engine_id, model, provider_ref, {USAGE_SUMS}
             FROM usage_events WHERE {SINCE}
             GROUP BY feature_id, engine_id, model, provider_ref
             ORDER BY prompt_tokens + completion_tokens DESC, feature_id"
        ))
        .bind(days)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// One row per day with any activity, oldest first.
    pub async fn daily(&self, days: i64) -> Result<Vec<UsageDay>, KeaError> {
        let rows = sqlx::query_as::<_, UsageDay>(&format!(
            "SELECT date(created_at) AS day, {USAGE_SUMS}
             FROM usage_events WHERE {SINCE}
             GROUP BY day ORDER BY day"
        ))
        .bind(days)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Drops the whole ledger. The counts are the only thing in it, so there
    /// is nothing to keep once the user has asked for it gone.
    pub async fn clear(&self) -> Result<u64, KeaError> {
        let result = sqlx::query("DELETE FROM usage_events")
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub async fn prune_older_than_days(&self, days: i64) -> Result<u64, KeaError> {
        let result = sqlx::query(
            "DELETE FROM usage_events WHERE created_at < datetime('now', printf('-%d days', ?))",
        )
        .bind(days)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_data_migrations};

    async fn repo() -> UsageRepo {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        UsageRepo::new(pool)
    }

    fn event(feature: &str, prompt: Option<i64>, completion: Option<i64>) -> NewUsageEvent {
        NewUsageEvent {
            model: Some("gpt-4o-mini".into()),
            provider_ref: Some("openai".into()),
            prompt_tokens: prompt,
            completion_tokens: completion,
            ..NewUsageEvent::new(feature, "openai")
        }
    }

    #[tokio::test]
    async fn totals_group_by_feature_and_model() {
        let repo = repo().await;
        repo.record(&event("rewrite", Some(10), Some(20)))
            .await
            .unwrap();
        repo.record(&event("rewrite", Some(1), Some(2)))
            .await
            .unwrap();
        repo.record(&event("dictation", Some(5), Some(5)))
            .await
            .unwrap();

        let totals = repo.totals(30).await.unwrap();
        assert_eq!(totals.len(), 2);
        // Ordered by tokens spent, so the row that costs the most is first.
        assert_eq!(totals[0].feature_id, "rewrite");
        assert_eq!(totals[0].calls, 2);
        assert_eq!(totals[0].prompt_tokens, 11);
        assert_eq!(totals[0].completion_tokens, 22);
        assert_eq!(totals[0].total_tokens(), 33);
        assert_eq!(totals[1].feature_id, "dictation");
    }

    /// The whole point of the NULL columns: a call nobody costed must not
    /// silently read as a free one.
    #[tokio::test]
    async fn unreported_calls_are_counted_not_zeroed() {
        let repo = repo().await;
        repo.record(&event("rewrite", Some(10), Some(20)))
            .await
            .unwrap();
        repo.record(&event("rewrite", None, None)).await.unwrap();

        let totals = repo.totals(30).await.unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].calls, 2);
        assert_eq!(totals[0].unreported_calls, 1);
        // The sums are the reported call's alone — a floor, not a total.
        assert_eq!(totals[0].total_tokens(), 30);
    }

    #[tokio::test]
    async fn daily_rolls_up_one_row_per_day() {
        let repo = repo().await;
        repo.record(&event("rewrite", Some(10), Some(20)))
            .await
            .unwrap();
        repo.record(&event("dictation", None, None)).await.unwrap();

        let days = repo.daily(30).await.unwrap();
        assert_eq!(days.len(), 1, "both calls are today");
        assert_eq!(days[0].calls, 2);
        assert_eq!(days[0].prompt_tokens, 10);
        assert_eq!(days[0].day.len(), 10, "YYYY-MM-DD");
    }

    #[tokio::test]
    async fn the_window_excludes_older_rows() {
        let repo = repo().await;
        repo.record(&event("rewrite", Some(10), Some(20)))
            .await
            .unwrap();
        sqlx::query("UPDATE usage_events SET created_at = datetime('now', '-40 days')")
            .execute(&repo.pool)
            .await
            .unwrap();

        assert!(repo.totals(30).await.unwrap().is_empty());
        assert!(repo.daily(30).await.unwrap().is_empty());
        assert_eq!(repo.totals(90).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn prune_and_clear_remove_rows() {
        let repo = repo().await;
        repo.record(&event("rewrite", Some(1), Some(1)))
            .await
            .unwrap();
        assert_eq!(repo.prune_older_than_days(30).await.unwrap(), 0);
        assert_eq!(repo.clear().await.unwrap(), 1);
        assert!(repo.totals(365).await.unwrap().is_empty());
    }

    /// The action link is `ON DELETE SET NULL`: pruning History must not take
    /// the spend with it, or a cost view would quietly shrink as old runs age
    /// out.
    #[tokio::test]
    async fn deleting_the_action_keeps_the_usage_row() {
        let repo = repo().await;
        let action_id: i64 = sqlx::query_scalar(
            "INSERT INTO actions(feature_id, command, engine_id, status)
             VALUES('rewrite', 'rewrite_selection', 'openai', 'ok') RETURNING id",
        )
        .fetch_one(&repo.pool)
        .await
        .unwrap();
        repo.record(&NewUsageEvent {
            action_id: Some(action_id),
            ..event("rewrite", Some(3), Some(4))
        })
        .await
        .unwrap();

        sqlx::query("DELETE FROM actions WHERE id = ?")
            .bind(action_id)
            .execute(&repo.pool)
            .await
            .unwrap();

        let totals = repo.totals(30).await.unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].total_tokens(), 7);
    }
}
