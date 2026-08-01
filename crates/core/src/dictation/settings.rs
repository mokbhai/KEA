use serde::{Deserialize, Serialize};

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

const KEY_POST_PROCESS: &str = "dictation.post_process";
const KEY_ACTIVE_MODEL: &str = "dictation.active_model";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictationSettings {
    pub post_process: bool,
    pub active_model: Option<String>,
}

pub struct DictationSettingsRepo {
    settings: SettingsRepo,
}

impl DictationSettingsRepo {
    pub fn new(settings: SettingsRepo) -> Self {
        Self { settings }
    }

    pub async fn get(&self) -> Result<DictationSettings, KeaError> {
        Ok(DictationSettings {
            post_process: self
                .settings
                .get(KEY_POST_PROCESS)
                .await?
                .unwrap_or(false),
            // Stored as an Option, so a cleared model is the JSON literal
            // `null` rather than a missing row — read it back as one and
            // flatten, or every later read of these settings would fail.
            active_model: self
                .settings
                .get::<Option<String>>(KEY_ACTIVE_MODEL)
                .await?
                .flatten(),
        })
    }

    pub async fn set(&self, cfg: &DictationSettings) -> Result<(), KeaError> {
        self.settings
            .set(KEY_POST_PROCESS, &cfg.post_process)
            .await?;
        self.settings
            .set(KEY_ACTIVE_MODEL, &cfg.active_model)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn dictation_settings_roundtrip() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = DictationSettingsRepo::new(SettingsRepo::new(pool));
        let cfg = DictationSettings {
            post_process: true,
            active_model: Some("ggml-base.en".into()),
        };
        repo.set(&cfg).await.unwrap();
        assert_eq!(repo.get().await.unwrap(), cfg);
    }

    #[tokio::test]
    async fn clearing_the_active_model_survives_a_reread() {
        // Clearing writes the JSON literal `null`; reading it back must yield
        // None instead of failing to deserialize.
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = DictationSettingsRepo::new(SettingsRepo::new(pool));
        repo.set(&DictationSettings {
            post_process: true,
            active_model: Some("ggml-base.en".into()),
        })
        .await
        .unwrap();
        repo.set(&DictationSettings {
            post_process: true,
            active_model: None,
        })
        .await
        .unwrap();

        let got = repo.get().await.unwrap();
        assert_eq!(got.active_model, None);
        assert!(got.post_process);
    }
}
