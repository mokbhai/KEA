use sqlx::SqlitePool;

use crate::error::KeaError;
use crate::store::bindings::Binding;
use crate::store::settings::SettingsRepo;

/// One saved instruction, and optionally the LLM it wants.
///
/// The three `llm_*` columns are the same override an app profile carries, at
/// a different scope: bindings are per capability slot, so without this every
/// preset shares one model and "cheap model for grammar, expensive one for a
/// hard rewrite" is unexpressible. `None` everywhere means "use whatever the
/// rewrite slot is bound to", which is what every preset written before this
/// column existed means.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct RewritePreset {
    pub id: String,
    pub name: String,
    pub instruction: String,
    #[serde(default)]
    pub llm_engine_id: Option<String>,
    #[serde(default)]
    pub llm_model: Option<String>,
    #[serde(default)]
    pub llm_provider_ref: Option<String>,
}

impl RewritePreset {
    /// A preset with no LLM of its own — the shape every caller that only
    /// cares about the instruction wants to write.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        instruction: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            instruction: instruction.into(),
            llm_engine_id: None,
            llm_model: None,
            llm_provider_ref: None,
        }
    }

    /// The LLM this preset asks for, or `None` to use the slot's binding.
    ///
    /// Same rule as [`crate::app_context::AppProfile::llm_binding`], through
    /// the same helper: an engine id is what makes an override resolvable.
    pub fn llm_binding(&self) -> Option<Binding> {
        Binding::from_parts(
            self.llm_engine_id.clone(),
            self.llm_model.clone(),
            self.llm_provider_ref.clone(),
        )
    }
}

/// The preset columns, spelled once. Three queries select them and a fourth
/// would be the one that forgets the new ones.
const PRESET_COLUMNS: &str = "id, name, instruction, llm_engine_id, llm_model, llm_provider_ref";

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
            "INSERT INTO rewrite_presets(id, name, instruction, llm_engine_id, llm_model, llm_provider_ref)
             VALUES(?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
               name = excluded.name,
               instruction = excluded.instruction,
               llm_engine_id = excluded.llm_engine_id,
               llm_model = excluded.llm_model,
               llm_provider_ref = excluded.llm_provider_ref",
        )
        .bind(&p.id)
        .bind(&p.name)
        .bind(&p.instruction)
        .bind(&p.llm_engine_id)
        .bind(&p.llm_model)
        .bind(&p.llm_provider_ref)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<RewritePreset>, KeaError> {
        let rows = sqlx::query_as::<_, RewritePreset>(&format!(
            "SELECT {PRESET_COLUMNS} FROM rewrite_presets ORDER BY sort_order, name"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn get(&self, id: &str) -> Result<Option<RewritePreset>, KeaError> {
        let row = sqlx::query_as::<_, RewritePreset>(&format!(
            "SELECT {PRESET_COLUMNS} FROM rewrite_presets WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
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
        repo.upsert(&RewritePreset::new("p1", "Formal", "Be formal"))
            .await
            .unwrap();
        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Formal");
        // A preset written without an LLM inherits the slot binding; that is
        // what every preset saved before the columns existed has to keep
        // meaning.
        assert_eq!(all[0].llm_binding(), None);
    }

    #[tokio::test]
    async fn preset_llm_override_round_trips() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = PresetRepo::new(pool);
        let mut preset = RewritePreset::new("p1", "Hard rewrite", "Rework this properly");
        preset.llm_engine_id = Some("openai".into());
        preset.llm_model = Some("gpt-5".into());
        preset.llm_provider_ref = Some("work".into());
        repo.upsert(&preset).await.unwrap();

        let stored = repo.get("p1").await.unwrap().unwrap();
        assert_eq!(stored, preset);
        let binding = stored.llm_binding().unwrap();
        assert_eq!(binding.engine_id, "openai");
        assert_eq!(binding.model.as_deref(), Some("gpt-5"));
        assert_eq!(binding.provider_ref.as_deref(), Some("work"));

        // An upsert that clears the override has to clear it in the row too,
        // or "back to the default AI" would be unreachable from the UI.
        repo.upsert(&RewritePreset::new(
            "p1",
            "Hard rewrite",
            "Rework this properly",
        ))
        .await
        .unwrap();
        assert_eq!(repo.get("p1").await.unwrap().unwrap().llm_binding(), None);
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
