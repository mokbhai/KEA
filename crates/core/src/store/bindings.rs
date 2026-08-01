use sqlx::SqlitePool;
use crate::error::KeaError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
}

pub struct BindingRepo { pool: SqlitePool }

impl BindingRepo {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub async fn get(&self, feature_id: &str, slot: &str) -> Result<Option<Binding>, KeaError> {
        let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT engine_id, model, provider_ref FROM bindings
             WHERE feature_id = ? AND slot = ?")
            .bind(feature_id).bind(slot).fetch_optional(&self.pool).await?;
        Ok(row.map(|(engine_id, model, provider_ref)| Binding { engine_id, model, provider_ref }))
    }

    pub async fn set(&self, feature_id: &str, slot: &str, b: Binding) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO bindings(feature_id, slot, engine_id, model, provider_ref)
             VALUES(?, ?, ?, ?, ?)
             ON CONFLICT(feature_id, slot) DO UPDATE SET
               engine_id = excluded.engine_id, model = excluded.model,
               provider_ref = excluded.provider_ref")
            .bind(feature_id).bind(slot).bind(b.engine_id).bind(b.model).bind(b.provider_ref)
            .execute(&self.pool).await?;
        Ok(())
    }

    pub async fn delete(&self, feature_id: &str, slot: &str) -> Result<(), KeaError> {
        sqlx::query("DELETE FROM bindings WHERE feature_id = ? AND slot = ?")
            .bind(feature_id).bind(slot)
            .execute(&self.pool).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    async fn repo() -> BindingRepo {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        BindingRepo::new(pool)
    }

    #[tokio::test]
    async fn delete_removes_only_the_matching_row() {
        let repo = repo().await;
        repo.set("rewrite", "llm", Binding {
            engine_id: "openai".into(), model: None, provider_ref: None }).await.unwrap();
        repo.set("default", "llm", Binding {
            engine_id: "local-llm".into(), model: None, provider_ref: None }).await.unwrap();

        repo.delete("default", "llm").await.unwrap();

        assert!(repo.get("default", "llm").await.unwrap().is_none());
        assert!(repo.get("rewrite", "llm").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_of_missing_row_is_a_no_op() {
        let repo = repo().await;
        repo.delete("default", "tts").await.unwrap();
        assert!(repo.get("default", "tts").await.unwrap().is_none());
    }
}
