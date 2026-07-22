use sqlx::SqlitePool;
use crate::error::KeaError;

pub struct ActionRepo { pool: SqlitePool }

pub struct NewAction {
    pub feature_id: String,
    pub command: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ActionRow {
    pub id: i64,
    pub feature_id: String,
    pub command: String,
    pub engine_id: String,
    pub status: String,
}

#[derive(serde::Serialize)]
pub struct ActionDetail {
    pub id: i64,
    pub feature_id: String,
    pub command: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

impl ActionRepo {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub async fn record(&self, a: NewAction) -> Result<i64, KeaError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO actions(feature_id, command, engine_id, model, provider_ref)
             VALUES(?, ?, ?, ?, ?) RETURNING id")
            .bind(a.feature_id).bind(a.command).bind(a.engine_id)
            .bind(a.model).bind(a.provider_ref)
            .fetch_one(&self.pool).await?;
        Ok(id)
    }

    pub async fn recent(&self, limit: i64) -> Result<Vec<ActionRow>, KeaError> {
        let rows = sqlx::query_as::<_, (i64, String, String, String, String)>(
            "SELECT id, feature_id, command, engine_id, status
             FROM actions ORDER BY id DESC LIMIT ?")
            .bind(limit).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(id, feature_id, command, engine_id, status)|
            ActionRow { id, feature_id, command, engine_id, status }).collect())
    }

    pub async fn finish(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), KeaError> {
        sqlx::query(
            "UPDATE actions SET status = ?, error = ?, finished_at = datetime('now') WHERE id = ?",
        )
        .bind(status)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn search(&self, query: &str, limit: i64) -> Result<Vec<ActionRow>, KeaError> {
        let pattern = format!("%{query}%");
        let rows = sqlx::query_as::<_, (i64, String, String, String, String)>(
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
        Ok(rows
            .into_iter()
            .map(|(id, feature_id, command, engine_id, status)| ActionRow {
                id,
                feature_id,
                command,
                engine_id,
                status,
            })
            .collect())
    }

    pub async fn get(&self, id: i64) -> Result<Option<ActionDetail>, KeaError> {
        let row = sqlx::query_as::<
            _,
            (
                i64,
                String,
                String,
                String,
                Option<String>,
                Option<String>,
                String,
                Option<String>,
                String,
                Option<String>,
            ),
        >(
            "SELECT id, feature_id, command, engine_id, model, provider_ref,
                    status, error, started_at, finished_at
             FROM actions WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(
                id,
                feature_id,
                command,
                engine_id,
                model,
                provider_ref,
                status,
                error,
                started_at,
                finished_at,
            )| ActionDetail {
                id,
                feature_id,
                command,
                engine_id,
                model,
                provider_ref,
                status,
                error,
                started_at,
                finished_at,
            },
        ))
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

    #[tokio::test]
    async fn record_then_list() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ActionRepo::new(pool);

        let id = repo.record(NewAction {
            feature_id: "demo".into(), command: "ping".into(),
            engine_id: "noop".into(), model: None, provider_ref: None,
        }).await.unwrap();
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
        repo.finish(id, "ok", None).await.unwrap();
        let rows = repo.recent(1).await.unwrap();
        assert_eq!(rows[0].status, "ok");
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
        repo.finish(id, "ok", None).await.unwrap();

        let detail = repo.get(id).await.unwrap().expect("action exists");
        assert_eq!(detail.feature_id, "tts");
        assert_eq!(detail.command, "read_selection");
        assert_eq!(detail.model, Some("tts-1".into()));
        assert_eq!(detail.status, "ok");
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
