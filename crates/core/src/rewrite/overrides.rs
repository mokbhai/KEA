use sqlx::SqlitePool;

use crate::error::KeaError;
use crate::rewrite::mode::RewriteMode;

pub struct PromptOverrideRepo {
    pool: SqlitePool,
}

impl PromptOverrideRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, mode: RewriteMode) -> Result<Option<String>, KeaError> {
        let row: Option<(String,)> = sqlx::query_as(
            "SELECT prompt FROM rewrite_prompt_overrides WHERE mode = ?",
        )
        .bind(mode.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(prompt,)| prompt))
    }

    pub async fn set(&self, mode: RewriteMode, prompt: &str) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO rewrite_prompt_overrides(mode, prompt) VALUES(?, ?)
             ON CONFLICT(mode) DO UPDATE SET prompt = excluded.prompt",
        )
        .bind(mode.as_str())
        .bind(prompt)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete(&self, mode: RewriteMode) -> Result<(), KeaError> {
        sqlx::query("DELETE FROM rewrite_prompt_overrides WHERE mode = ?")
            .bind(mode.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn override_roundtrip() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = PromptOverrideRepo::new(pool);
        repo.set(RewriteMode::Improve, "Custom improve prompt")
            .await
            .unwrap();
        assert_eq!(
            repo.get(RewriteMode::Improve).await.unwrap(),
            Some("Custom improve prompt".into())
        );
    }
}
