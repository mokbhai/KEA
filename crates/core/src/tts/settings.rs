use serde::{Deserialize, Serialize};

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

const KEY_ACTIVE_VOICE: &str = "tts.active_voice";
const KEY_ACTIVE_MODEL: &str = "tts.active_model";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TtsSettings {
    #[serde(default)]
    pub active_voice: Option<String>,
    #[serde(default)]
    pub active_model: Option<String>,
}

impl Default for TtsSettings {
    fn default() -> Self {
        Self {
            active_voice: None,
            active_model: None,
        }
    }
}

pub struct TtsSettingsRepo {
    settings: SettingsRepo,
}

impl TtsSettingsRepo {
    pub fn new(settings: SettingsRepo) -> Self {
        Self { settings }
    }

    pub async fn get(&self) -> Result<TtsSettings, KeaError> {
        Ok(TtsSettings {
            active_voice: self.settings.get(KEY_ACTIVE_VOICE).await?,
            active_model: self.settings.get(KEY_ACTIVE_MODEL).await?,
        })
    }

    pub async fn set(&self, cfg: &TtsSettings) -> Result<(), KeaError> {
        self.settings
            .set(KEY_ACTIVE_VOICE, &cfg.active_voice)
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
    async fn tts_settings_roundtrip() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = TtsSettingsRepo::new(SettingsRepo::new(pool));
        let cfg = TtsSettings {
            active_voice: Some("alloy".into()),
            active_model: Some("tts-1".into()),
        };
        repo.set(&cfg).await.unwrap();
        assert_eq!(repo.get().await.unwrap(), cfg);
    }

    #[tokio::test]
    async fn tts_settings_defaults() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = TtsSettingsRepo::new(SettingsRepo::new(pool));
        assert_eq!(repo.get().await.unwrap(), TtsSettings::default());
    }
}
