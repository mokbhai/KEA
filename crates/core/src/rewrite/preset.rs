use sqlx::SqlitePool;

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RewritePreset {
    pub id: String,
    pub name: String,
    pub instruction: String,
}

pub struct PresetRepo {
    pool: SqlitePool,
    settings: SettingsRepo,
}

impl PresetRepo {
    pub fn new(pool: SqlitePool) -> Self {
        let settings = SettingsRepo::new(pool.clone());
        Self { pool, settings }
    }

    pub async fn upsert(&self, p: &RewritePreset) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO rewrite_presets(id, name, instruction) VALUES(?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, instruction = excluded.instruction",
        )
        .bind(&p.id)
        .bind(&p.name)
        .bind(&p.instruction)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<RewritePreset>, KeaError> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            "SELECT id, name, instruction FROM rewrite_presets ORDER BY sort_order, name",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(id, name, instruction)| RewritePreset {
                id,
                name,
                instruction,
            })
            .collect())
    }

    pub async fn get(&self, id: &str) -> Result<Option<RewritePreset>, KeaError> {
        let row = sqlx::query_as::<_, (String, String, String)>(
            "SELECT id, name, instruction FROM rewrite_presets WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(id, name, instruction)| RewritePreset {
            id,
            name,
            instruction,
        }))
    }

    pub async fn delete(&self, id: &str) -> Result<(), KeaError> {
        sqlx::query("DELETE FROM rewrite_presets WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_active(&self, id: &str) -> Result<(), KeaError> {
        self.settings
            .set("rewrite.active_preset_id", &id.to_string())
            .await
    }

    pub async fn active_id(&self) -> Result<Option<String>, KeaError> {
        self.settings.get("rewrite.active_preset_id").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn preset_roundtrip() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = PresetRepo::new(pool.clone());
        repo.upsert(&RewritePreset {
            id: "p1".into(),
            name: "Formal".into(),
            instruction: "Be formal".into(),
        })
        .await
        .unwrap();
        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Formal");
    }

    #[tokio::test]
    async fn active_preset_roundtrips() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = PresetRepo::new(pool);
        repo.set_active("p1").await.unwrap();
        assert_eq!(repo.active_id().await.unwrap(), Some("p1".into()));
    }
}
