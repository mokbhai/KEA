use crate::error::KeaError;
use sqlx::SqlitePool;

/// How an entry in the `actions` ledger ended, as persisted in
/// `actions.status`. `Started` is the column's DEFAULT, written by the INSERT
/// and overwritten by [`ActionRepo::finish`]. Spelled once here so callers bind
/// a variant, not a literal — see [`crate::store::meetings::MeetingStatus`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Started,
    Ok,
    Error,
    /// The user called it off — a dismissed prompt palette, a stopped file
    /// transcription. Distinct from `Error` on purpose: a run the user ended
    /// is not a fault, and reporting it as one trains people to ignore the
    /// colour that is supposed to mean something went wrong.
    Cancelled,
}

impl ActionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionStatus::Started => "started",
            ActionStatus::Ok => "ok",
            ActionStatus::Error => "error",
            ActionStatus::Cancelled => "cancelled",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "started" => Some(ActionStatus::Started),
            "ok" => Some(ActionStatus::Ok),
            "error" => Some(ActionStatus::Error),
            "cancelled" => Some(ActionStatus::Cancelled),
            _ => None,
        }
    }
}

impl TryFrom<String> for ActionStatus {
    type Error = KeaError;

    fn try_from(s: String) -> Result<Self, KeaError> {
        ActionStatus::from_str(&s)
            .ok_or_else(|| KeaError::Other(format!("unknown action status {s:?}")))
    }
}

/// What History shows for a run that never reported an outcome.
pub const INTERRUPTED_MESSAGE: &str = "interrupted: KEA closed before this finished";

pub struct ActionRepo {
    pool: SqlitePool,
}

pub struct NewAction {
    pub feature_id: String,
    pub command: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
}

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct ActionRow {
    pub id: i64,
    pub feature_id: String,
    pub command: String,
    pub engine_id: String,
    #[sqlx(try_from = "String")]
    pub status: ActionStatus,
}

#[derive(serde::Serialize, sqlx::FromRow)]
pub struct ActionDetail {
    pub id: i64,
    pub feature_id: String,
    pub command: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
    #[sqlx(try_from = "String")]
    pub status: ActionStatus,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

impl ActionRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn record(&self, a: NewAction) -> Result<i64, KeaError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO actions(feature_id, command, engine_id, model, provider_ref)
             VALUES(?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(a.feature_id)
        .bind(a.command)
        .bind(a.engine_id)
        .bind(a.model)
        .bind(a.provider_ref)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn recent(&self, limit: i64) -> Result<Vec<ActionRow>, KeaError> {
        let rows = sqlx::query_as::<_, ActionRow>(
            "SELECT id, feature_id, command, engine_id, status
             FROM actions ORDER BY id DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn finish(
        &self,
        id: i64,
        status: ActionStatus,
        error: Option<&str>,
    ) -> Result<(), KeaError> {
        sqlx::query(
            "UPDATE actions SET status = ?, error = ?, finished_at = datetime('now') WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Closes `id` from a context that cannot await — a `Drop` running while a
    /// task unwinds from a panic. Returns whether the write was scheduled.
    ///
    /// Best effort by construction: with no tokio runtime to hand the write to
    /// (shutdown), or a runtime that stops before it runs, the row stays open
    /// and [`ActionRepo::close_interrupted`] reaps it at the next launch.
    pub fn finish_detached(&self, id: i64, status: ActionStatus, error: String) -> bool {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return false;
        };
        let repo = ActionRepo::new(self.pool.clone());
        runtime.spawn(async move {
            if let Err(e) = repo.finish(id, status, Some(&error)).await {
                tracing::warn!(error = %e, action_id = id, "could not close an abandoned action");
            }
        });
        true
    }

    /// Marks every row still `started` as an error, for the start of a launch.
    ///
    /// At that moment no run can be in flight — this process has not started
    /// one — so a row still `started` belongs to a process that died under it
    /// (a quit mid-dictation, a crash, a killed dev build) and will otherwise
    /// read as "running" in History forever. Must run before anything can
    /// record a new action, or it would close a live row.
    pub async fn close_interrupted(&self) -> Result<u64, KeaError> {
        let result = sqlx::query(
            "UPDATE actions
             SET status = 'error', error = ?, finished_at = datetime('now')
             WHERE status = 'started'",
        )
        .bind(INTERRUPTED_MESSAGE)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }

    pub async fn search(&self, query: &str, limit: i64) -> Result<Vec<ActionRow>, KeaError> {
        let pattern = format!("%{query}%");
        let rows = sqlx::query_as::<_, ActionRow>(
            "SELECT id, feature_id, command, engine_id, status
             FROM actions
             WHERE feature_id LIKE ? OR command LIKE ? OR engine_id LIKE ?
             ORDER BY id DESC LIMIT ?",
        )
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn get(&self, id: i64) -> Result<Option<ActionDetail>, KeaError> {
        let row = sqlx::query_as::<_, ActionDetail>(
            "SELECT id, feature_id, command, engine_id, model, provider_ref,
                    status, error, started_at, finished_at
             FROM actions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn prune_older_than_days(&self, days: i64) -> Result<u64, KeaError> {
        let result = sqlx::query(
            "DELETE FROM actions
             WHERE started_at < datetime('now', printf('-%d days', ?))",
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

    #[test]
    fn status_keeps_its_stored_spellings() {
        // `started` is the column's DEFAULT; `ok`/`error` are what History
        // shows. The enum must not re-spell any of the three.
        for (status, text) in [
            (ActionStatus::Started, "started"),
            (ActionStatus::Ok, "ok"),
            (ActionStatus::Error, "error"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(ActionStatus::from_str(text), Some(status));
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{text}\"")
            );
        }
    }

    #[tokio::test]
    async fn record_then_list() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ActionRepo::new(pool);

        let id = repo
            .record(NewAction {
                feature_id: "demo".into(),
                command: "ping".into(),
                engine_id: "noop".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap();
        assert!(id > 0);

        let rows = repo.recent(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature_id, "demo");
    }

    #[tokio::test]
    async fn finish_marks_action_done() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ActionRepo::new(pool);
        let id = repo
            .record(NewAction {
                feature_id: "rewrite".into(),
                command: "rewrite".into(),
                engine_id: "openai".into(),
                model: Some("gpt-4o-mini".into()),
                provider_ref: Some("openai".into()),
            })
            .await
            .unwrap();
        repo.finish(id, ActionStatus::Ok, None).await.unwrap();
        let rows = repo.recent(1).await.unwrap();
        assert_eq!(rows[0].status, ActionStatus::Ok);
    }

    fn new_action(feature: &str) -> NewAction {
        NewAction {
            feature_id: feature.into(),
            command: "run".into(),
            engine_id: "noop".into(),
            model: None,
            provider_ref: None,
        }
    }

    /// Rows 6, 65, 67 and 438 of a real data.db sat at `started` for weeks
    /// because the process that opened them quit mid-run.
    #[tokio::test]
    async fn close_interrupted_reaps_open_rows_and_leaves_finished_ones() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ActionRepo::new(pool);
        let open = repo.record(new_action("dictation")).await.unwrap();
        let done = repo.record(new_action("dictation")).await.unwrap();
        repo.finish(done, ActionStatus::Ok, None).await.unwrap();
        let failed = repo.record(new_action("rewrite")).await.unwrap();
        repo.finish(failed, ActionStatus::Error, Some("boom"))
            .await
            .unwrap();

        assert_eq!(repo.close_interrupted().await.unwrap(), 1);

        let row = repo.get(open).await.unwrap().unwrap();
        assert_eq!(row.status, ActionStatus::Error);
        assert_eq!(row.error.as_deref(), Some(INTERRUPTED_MESSAGE));
        assert!(row.finished_at.is_some());
        assert_eq!(
            repo.get(done).await.unwrap().unwrap().status,
            ActionStatus::Ok
        );
        assert_eq!(
            repo.get(failed).await.unwrap().unwrap().error.as_deref(),
            Some("boom")
        );
        assert_eq!(repo.close_interrupted().await.unwrap(), 0, "idempotent");
    }

    #[tokio::test]
    async fn finish_detached_closes_the_row_from_a_sync_context() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ActionRepo::new(pool);
        let id = repo.record(new_action("dictation")).await.unwrap();

        assert!(repo.finish_detached(id, ActionStatus::Error, "abandoned".into()));

        for _ in 0..50 {
            if repo.get(id).await.unwrap().unwrap().status != ActionStatus::Started {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let row = repo.get(id).await.unwrap().unwrap();
        assert_eq!(row.status, ActionStatus::Error);
        assert_eq!(row.error.as_deref(), Some("abandoned"));
    }

    #[tokio::test]
    async fn search_matches_feature_id_fragment() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ActionRepo::new(pool);

        repo.record(NewAction {
            feature_id: "dictation".into(),
            command: "transcribe".into(),
            engine_id: "noop-stt".into(),
            model: None,
            provider_ref: None,
        })
        .await
        .unwrap();
        repo.record(NewAction {
            feature_id: "rewrite".into(),
            command: "rewrite".into(),
            engine_id: "openai".into(),
            model: None,
            provider_ref: None,
        })
        .await
        .unwrap();

        let hits = repo.search("dict", 10).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].feature_id, "dictation");
    }

    #[tokio::test]
    async fn get_returns_action_detail() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ActionRepo::new(pool);
        let id = repo
            .record(NewAction {
                feature_id: "tts".into(),
                command: "read_selection".into(),
                engine_id: "noop-tts".into(),
                model: Some("tts-1".into()),
                provider_ref: Some("openai".into()),
            })
            .await
            .unwrap();
        repo.finish(id, ActionStatus::Ok, None).await.unwrap();

        let detail = repo.get(id).await.unwrap().expect("action exists");
        assert_eq!(detail.feature_id, "tts");
        assert_eq!(detail.command, "read_selection");
        assert_eq!(detail.model, Some("tts-1".into()));
        assert_eq!(detail.status, ActionStatus::Ok);
        assert!(detail.finished_at.is_some());
    }

    #[tokio::test]
    async fn prune_older_than_days_removes_stale_rows() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ActionRepo::new(pool.clone());
        let id = repo
            .record(NewAction {
                feature_id: "old".into(),
                command: "ping".into(),
                engine_id: "noop".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap();
        sqlx::query("UPDATE actions SET started_at = datetime('now', '-90 days') WHERE id = ?")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();

        let pruned = repo.prune_older_than_days(30).await.unwrap();
        assert_eq!(pruned, 1);
        assert!(repo.get(id).await.unwrap().is_none());
    }
}
