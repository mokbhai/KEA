use serde::{Deserialize, Serialize};

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

const KEY_POST_PROCESS: &str = "dictation.post_process";
const KEY_ACTIVE_MODEL: &str = "dictation.active_model";
const KEY_HOLD_TO_TALK: &str = "dictation.hold_to_talk";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DictationSettings {
    pub post_process: bool,
    pub active_model: Option<String>,
    /// Hold ⌥⇧ to record, release to transcribe and insert.
    ///
    /// Off by default, and a config written before this existed simply has no
    /// row for the key — which `get` reads as off. That is the whole migration:
    /// these settings are one row per key rather than a serialized blob, so an
    /// added field cannot fail to deserialize an older config.
    #[serde(default)]
    pub hold_to_talk: bool,
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
            hold_to_talk: self.settings.get(KEY_HOLD_TO_TALK).await?.unwrap_or(false),
        })
    }

    pub async fn set(&self, cfg: &DictationSettings) -> Result<(), KeaError> {
        self.settings
            .set(KEY_POST_PROCESS, &cfg.post_process)
            .await?;
        self.settings
            .set(KEY_ACTIVE_MODEL, &cfg.active_model)
            .await?;
        self.settings
            .set(KEY_HOLD_TO_TALK, &cfg.hold_to_talk)
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
            hold_to_talk: true,
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
            hold_to_talk: false,
        })
        .await
        .unwrap();
        repo.set(&DictationSettings {
            post_process: true,
            active_model: None,
            hold_to_talk: false,
        })
        .await
        .unwrap();

        let got = repo.get().await.unwrap();
        assert_eq!(got.active_model, None);
        assert!(got.post_process);
    }

    /// A config stored before `hold_to_talk` existed has no row for it, which
    /// must read back as "off" rather than failing the whole settings read and
    /// taking dictation's model and clean-up toggle down with it.
    #[tokio::test]
    async fn a_config_written_before_hold_to_talk_existed_still_reads() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool);
        // Exactly what 0.2.0 wrote: the two keys it knew about, and nothing else.
        settings.set("dictation.post_process", &true).await.unwrap();
        settings
            .set("dictation.active_model", &Some("ggml-base.en".to_string()))
            .await
            .unwrap();

        let got = DictationSettingsRepo::new(settings).get().await.unwrap();
        assert!(got.post_process);
        assert_eq!(got.active_model.as_deref(), Some("ggml-base.en"));
        assert!(!got.hold_to_talk, "a missing row means the mode is off");
    }
}
