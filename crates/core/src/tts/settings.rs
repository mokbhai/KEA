use serde::{Deserialize, Serialize};

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

const KEY_ACTIVE_VOICE: &str = "tts.active_voice";
const KEY_ACTIVE_MODEL: &str = "tts.active_model";
const KEY_SPEED: &str = "tts.speed";

/// A rate of 1.0 is the voice's natural pace. Named rather than inlined so
/// the default is the same number in the reader, the serde default and the
/// `Default` impl.
const DEFAULT_SPEED: f32 = 1.0;

/// Not `Eq`: `speed` is a float. Nothing compares these for equality outside
/// tests, where `PartialEq` is enough.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TtsSettings {
    /// The chosen speaker, by name — see `kea_engines::traits::TtsOpts::voice`.
    #[serde(default)]
    pub active_voice: Option<String>,
    #[serde(default)]
    pub active_model: Option<String>,
    /// Rate multiplier.
    ///
    /// Stored as given and brought into range where it is used, so the bounds
    /// live in one place (`kea_infer::clamp_tts_speed`) rather than being
    /// re-stated by every writer. A config written before this key existed has
    /// no row, which reads as the default — the whole migration, as for
    /// `DictationSettings`.
    #[serde(default = "default_speed")]
    pub speed: f32,
}

fn default_speed() -> f32 {
    DEFAULT_SPEED
}

impl Default for TtsSettings {
    fn default() -> Self {
        Self {
            active_voice: None,
            active_model: None,
            speed: DEFAULT_SPEED,
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
            active_voice: self.settings.get_optional(KEY_ACTIVE_VOICE).await?,
            active_model: self.settings.get_optional(KEY_ACTIVE_MODEL).await?,
            speed: self.settings.get(KEY_SPEED).await?.unwrap_or(DEFAULT_SPEED),
        })
    }

    pub async fn set(&self, cfg: &TtsSettings) -> Result<(), KeaError> {
        self.settings
            .set(KEY_ACTIVE_VOICE, &cfg.active_voice)
            .await?;
        self.settings
            .set(KEY_ACTIVE_MODEL, &cfg.active_model)
            .await?;
        self.settings.set(KEY_SPEED, &cfg.speed).await?;
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
            speed: 1.25,
        };
        repo.set(&cfg).await.unwrap();
        assert_eq!(repo.get().await.unwrap(), cfg);
    }

    #[tokio::test]
    async fn clearing_voice_and_model_survives_a_reread() {
        // Clearing writes the JSON literal `null`; reading it back must yield
        // None instead of failing to deserialize.
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = TtsSettingsRepo::new(SettingsRepo::new(pool));
        repo.set(&TtsSettings {
            active_voice: Some("alloy".into()),
            active_model: Some("tts-1".into()),
            ..Default::default()
        })
        .await
        .unwrap();
        repo.set(&TtsSettings::default()).await.unwrap();

        assert_eq!(repo.get().await.unwrap(), TtsSettings::default());
    }

    #[tokio::test]
    async fn tts_settings_defaults() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = TtsSettingsRepo::new(SettingsRepo::new(pool));
        let defaults = repo.get().await.unwrap();
        assert_eq!(defaults, TtsSettings::default());
        assert_eq!(defaults.speed, 1.0, "the natural pace, not 0.0");
    }

    /// A config stored before the rate existed has no row for it. That must
    /// read back as the natural pace rather than as `f32::default()`, which
    /// is 0.0 and makes the synthesizer emit nothing at all.
    #[tokio::test]
    async fn a_config_written_before_the_rate_existed_still_speaks() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool);
        settings
            .set("tts.active_voice", &Some("alloy".to_string()))
            .await
            .unwrap();

        let got = TtsSettingsRepo::new(settings).get().await.unwrap();
        assert_eq!(got.active_voice.as_deref(), Some("alloy"));
        assert_eq!(got.speed, 1.0);
    }

    /// The rate is stored as given: bringing it into range is the engine's
    /// job, in one place, so a value written by an older or newer build is
    /// never silently rewritten here.
    #[tokio::test]
    async fn the_rate_survives_a_roundtrip_unchanged() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = TtsSettingsRepo::new(SettingsRepo::new(pool));
        repo.set(&TtsSettings {
            speed: 0.75,
            ..Default::default()
        })
        .await
        .unwrap();
        assert_eq!(repo.get().await.unwrap().speed, 0.75);
    }
}
