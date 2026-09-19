use crate::error::KeaError;
use sqlx::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Binding {
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
}

impl Binding {
    /// A binding from three nullable override columns, or `None` when they
    /// name nothing resolvable.
    ///
    /// The rule is the engine id: a model or a provider ref with no engine
    /// names no engine to send the request to, so the override inherits
    /// instead of half-applying. Two tables carry exactly these three columns
    /// for exactly this purpose — `app_profiles` and `rewrite_presets` — and
    /// this is the one place that says what a half-filled row means, so the
    /// two cannot come to disagree about it.
    pub fn from_parts(
        engine_id: Option<String>,
        model: Option<String>,
        provider_ref: Option<String>,
    ) -> Option<Self> {
        Some(Self {
            engine_id: engine_id?,
            model,
            provider_ref,
        })
    }
}

pub struct BindingRepo {
    pool: SqlitePool,
}

impl BindingRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, feature_id: &str, slot: &str) -> Result<Option<Binding>, KeaError> {
        let row = sqlx::query_as::<_, Binding>(
            "SELECT engine_id, model, provider_ref FROM bindings
             WHERE feature_id = ? AND slot = ?",
        )
        .bind(feature_id)
        .bind(slot)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn set(&self, feature_id: &str, slot: &str, b: Binding) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO bindings(feature_id, slot, engine_id, model, provider_ref)
             VALUES(?, ?, ?, ?, ?)
             ON CONFLICT(feature_id, slot) DO UPDATE SET
               engine_id = excluded.engine_id, model = excluded.model,
               provider_ref = excluded.provider_ref",
        )
        .bind(feature_id)
        .bind(slot)
        .bind(b.engine_id)
        .bind(b.model)
        .bind(b.provider_ref)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn delete(&self, feature_id: &str, slot: &str) -> Result<(), KeaError> {
        sqlx::query("DELETE FROM bindings WHERE feature_id = ? AND slot = ?")
            .bind(feature_id)
            .bind(slot)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Drops every binding for `slot` that names `model_id` — the capability
    /// default and any per-feature override alike. Returns the feature ids
    /// whose rows were removed, sorted, so callers can log/report them.
    /// Used when a model's files are deleted: a row left behind would only
    /// surface as an inference failure much later.
    pub async fn delete_by_model(
        &self,
        slot: &str,
        model_id: &str,
    ) -> Result<Vec<String>, KeaError> {
        let feature_ids: Vec<String> = sqlx::query_scalar(
            "SELECT feature_id FROM bindings WHERE slot = ? AND model = ? ORDER BY feature_id",
        )
        .bind(slot)
        .bind(model_id)
        .fetch_all(&self.pool)
        .await?;
        if feature_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query("DELETE FROM bindings WHERE slot = ? AND model = ?")
            .bind(slot)
            .bind(model_id)
            .execute(&self.pool)
            .await?;
        Ok(feature_ids)
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
        repo.set(
            "rewrite",
            "llm",
            Binding {
                engine_id: "openai".into(),
                model: None,
                provider_ref: None,
            },
        )
        .await
        .unwrap();
        repo.set(
            "default",
            "llm",
            Binding {
                engine_id: "local-llm".into(),
                model: None,
                provider_ref: None,
            },
        )
        .await
        .unwrap();

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

    async fn seed_model_rows(repo: &BindingRepo) {
        for (feature, slot, model) in [
            ("default", "stt", Some("whisper-base")),
            ("dictation", "stt", Some("whisper-base")),
            ("meetings", "stt", Some("whisper-small")),
            ("default", "tts", Some("whisper-base")), // same id, other slot
            ("rewrite", "llm", None),
        ] {
            repo.set(
                feature,
                slot,
                Binding {
                    engine_id: "whisper".into(),
                    model: model.map(Into::into),
                    provider_ref: None,
                },
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn delete_by_model_clears_every_row_for_that_slot() {
        let repo = repo().await;
        seed_model_rows(&repo).await;

        let cleared = repo.delete_by_model("stt", "whisper-base").await.unwrap();

        assert_eq!(
            cleared,
            vec!["default".to_string(), "dictation".to_string()]
        );
        assert!(repo.get("default", "stt").await.unwrap().is_none());
        assert!(repo.get("dictation", "stt").await.unwrap().is_none());
        // Other models and other slots are untouched.
        assert!(repo.get("meetings", "stt").await.unwrap().is_some());
        assert!(repo.get("default", "tts").await.unwrap().is_some());
        assert!(repo.get("rewrite", "llm").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_by_model_without_matches_reports_nothing() {
        let repo = repo().await;
        seed_model_rows(&repo).await;
        assert!(repo
            .delete_by_model("stt", "not-installed")
            .await
            .unwrap()
            .is_empty());
        assert!(repo.get("default", "stt").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn delete_by_model_ignores_rows_without_a_model() {
        let repo = repo().await;
        repo.set(
            "default",
            "llm",
            Binding {
                engine_id: "openai".into(),
                model: None,
                provider_ref: None,
            },
        )
        .await
        .unwrap();
        assert!(repo
            .delete_by_model("llm", "gpt-4o")
            .await
            .unwrap()
            .is_empty());
        assert!(repo.get("default", "llm").await.unwrap().is_some());
    }

    #[test]
    fn from_parts_needs_an_engine_id() {
        // A model with no engine names nothing resolvable: the override has to
        // inherit rather than half-apply, and both override tables depend on
        // this being the answer.
        assert_eq!(
            Binding::from_parts(None, Some("gpt-4o".into()), Some("work".into())),
            None
        );
        assert_eq!(
            Binding::from_parts(Some("openai".into()), None, None),
            Some(Binding {
                engine_id: "openai".into(),
                model: None,
                provider_ref: None,
            })
        );
    }
}
