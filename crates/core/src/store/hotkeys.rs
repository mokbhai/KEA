use sqlx::SqlitePool;

use crate::error::KeaError;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HotkeyBindingRow {
    pub feature_id: String,
    pub command: String,
    pub accelerator: String,
}

pub struct HotkeyBindingRepo {
    pool: SqlitePool,
}

impl HotkeyBindingRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(
        &self,
        feature_id: &str,
        command: &str,
    ) -> Result<Option<HotkeyBindingRow>, KeaError> {
        let row = sqlx::query_as::<_, (String, String, String)>(
            "SELECT feature_id, command, accelerator FROM hotkey_bindings
             WHERE feature_id = ? AND command = ?",
        )
        .bind(feature_id)
        .bind(command)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(feature_id, command, accelerator)| HotkeyBindingRow {
            feature_id,
            command,
            accelerator,
        }))
    }

    pub async fn set(
        &self,
        feature_id: &str,
        command: &str,
        accelerator: &str,
    ) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO hotkey_bindings(feature_id, command, accelerator) VALUES(?, ?, ?)
             ON CONFLICT(feature_id, command) DO UPDATE SET accelerator = excluded.accelerator",
        )
        .bind(feature_id)
        .bind(command)
        .bind(accelerator)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<HotkeyBindingRow>, KeaError> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            "SELECT feature_id, command, accelerator FROM hotkey_bindings
             ORDER BY feature_id, command",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(feature_id, command, accelerator)| HotkeyBindingRow {
                feature_id,
                command,
                accelerator,
            })
            .collect())
    }

    pub async fn delete(&self, feature_id: &str, command: &str) -> Result<(), KeaError> {
        sqlx::query("DELETE FROM hotkey_bindings WHERE feature_id = ? AND command = ?")
            .bind(feature_id)
            .bind(command)
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
    async fn set_rewrite_hotkey() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = HotkeyBindingRepo::new(pool);
        repo.set("rewrite", "rewrite", "CommandOrControl+Shift+R")
            .await
            .unwrap();
        let row = repo.get("rewrite", "rewrite").await.unwrap().unwrap();
        assert_eq!(row.accelerator, "CommandOrControl+Shift+R");
    }

    #[tokio::test]
    async fn list_bindings() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = HotkeyBindingRepo::new(pool);
        repo.set("rewrite", "rewrite", "CommandOrControl+Shift+R")
            .await
            .unwrap();
        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 1);
    }
}
