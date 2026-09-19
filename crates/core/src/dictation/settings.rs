use serde::{Deserialize, Serialize};

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

const KEY_POST_PROCESS: &str = "dictation.post_process";
const KEY_ACTIVE_MODEL: &str = "dictation.active_model";
const KEY_HOLD_TO_TALK: &str = "dictation.hold_to_talk";
const KEY_INPUT_DEVICE: &str = "dictation.input_device";
const KEY_PREROLL: &str = "dictation.preroll";

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
    /// Which microphone to record from, by device name, or `None` for the
    /// system default.
    ///
    /// A name is the only handle the audio layer has, and names are neither
    /// unique nor stable across reboots, so a saved name that no longer
    /// matches anything falls back to the default rather than failing.
    #[serde(default)]
    pub input_device: Option<String>,
    /// Open the microphone as the hold chord is being assembled, so the speech
    /// before the hold threshold is not clipped.
    ///
    /// On by default, unlike `hold_to_talk`: the microphone is open for the
    /// few hundred milliseconds of a modifier press rather than all day, which
    /// is a claim worth making by default. Off is for anyone who would rather
    /// the mic never open without a deliberate recording.
    #[serde(default = "preroll_default")]
    pub preroll: bool,
}

/// A config written before the preroll existed has no row, and a payload from
/// an older frontend has no field. Both mean "the default", which is on.
fn preroll_default() -> bool {
    true
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
            post_process: self.settings.get(KEY_POST_PROCESS).await?.unwrap_or(false),
            active_model: self.settings.get_optional(KEY_ACTIVE_MODEL).await?,
            hold_to_talk: self.settings.get(KEY_HOLD_TO_TALK).await?.unwrap_or(false),
            input_device: self.settings.get_optional(KEY_INPUT_DEVICE).await?,
            preroll: self.settings.get(KEY_PREROLL).await?.unwrap_or(true),
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
        self.settings
            .set(KEY_INPUT_DEVICE, &cfg.input_device)
            .await?;
        self.settings.set(KEY_PREROLL, &cfg.preroll).await?;
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
            input_device: Some("Yeti".into()),
            preroll: false,
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
            input_device: None,
            preroll: true,
        })
        .await
        .unwrap();
        repo.set(&DictationSettings {
            post_process: true,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: true,
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
        assert_eq!(got.input_device, None, "no row means the default device");
        assert!(
            got.preroll,
            "the preroll defaults on, so a config that predates it gets it"
        );
    }

    /// Clearing the device picker writes the JSON literal `null`, which must
    /// read back as "the default" rather than failing the whole settings read.
    #[tokio::test]
    async fn clearing_the_input_device_survives_a_reread() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = DictationSettingsRepo::new(SettingsRepo::new(pool));
        let mut cfg = DictationSettings {
            post_process: false,
            active_model: None,
            hold_to_talk: false,
            input_device: Some("Yeti".into()),
            preroll: true,
        };
        repo.set(&cfg).await.unwrap();
        assert_eq!(
            repo.get().await.unwrap().input_device.as_deref(),
            Some("Yeti")
        );

        cfg.input_device = None;
        repo.set(&cfg).await.unwrap();
        assert_eq!(repo.get().await.unwrap().input_device, None);
    }

    /// The preroll is the one setting here that is on unless told otherwise,
    /// so "off" has to survive a write — an `unwrap_or(true)` over a missing
    /// row would otherwise quietly turn it back on.
    #[tokio::test]
    async fn switching_the_preroll_off_sticks() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = DictationSettingsRepo::new(SettingsRepo::new(pool));
        repo.set(&DictationSettings {
            post_process: false,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: false,
        })
        .await
        .unwrap();
        assert!(!repo.get().await.unwrap().preroll);
    }
}
