use serde::{Deserialize, Serialize};

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

const KEY_SEGMENT_DURATION: &str = "meetings.segment_duration_secs";
const KEY_PREFER_SYSTEM_AUDIO: &str = "meetings.prefer_system_audio";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingSettings {
    pub segment_duration_secs: u32,
    pub prefer_system_audio: bool,
}

impl Default for MeetingSettings {
    fn default() -> Self {
        Self {
            segment_duration_secs: 30,
            prefer_system_audio: true,
        }
    }
}

pub struct MeetingSettingsRepo {
    settings: SettingsRepo,
}

impl MeetingSettingsRepo {
    pub fn new(settings: SettingsRepo) -> Self {
        Self { settings }
    }

    pub async fn get(&self) -> Result<MeetingSettings, KeaError> {
        Ok(MeetingSettings {
            segment_duration_secs: self
                .settings
                .get(KEY_SEGMENT_DURATION)
                .await?
                .unwrap_or(30),
            prefer_system_audio: self
                .settings
                .get(KEY_PREFER_SYSTEM_AUDIO)
                .await?
                .unwrap_or(true),
        })
    }

    pub async fn set(&self, cfg: &MeetingSettings) -> Result<(), KeaError> {
        self.settings
            .set(KEY_SEGMENT_DURATION, &cfg.segment_duration_secs)
            .await?;
        self.settings
            .set(KEY_PREFER_SYSTEM_AUDIO, &cfg.prefer_system_audio)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn meeting_settings_roundtrip() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = MeetingSettingsRepo::new(SettingsRepo::new(pool));
        let cfg = MeetingSettings {
            segment_duration_secs: 45,
            prefer_system_audio: true,
        };
        repo.set(&cfg).await.unwrap();
        assert_eq!(repo.get().await.unwrap(), cfg);
    }

    #[tokio::test]
    async fn meeting_settings_defaults() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = MeetingSettingsRepo::new(SettingsRepo::new(pool));
        assert_eq!(repo.get().await.unwrap(), MeetingSettings::default());
    }
}
