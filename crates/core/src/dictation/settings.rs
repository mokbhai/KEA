use serde::{Deserialize, Serialize};

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

const KEY_POST_PROCESS: &str = "dictation.post_process";
const KEY_ACTIVE_MODEL: &str = "dictation.active_model";
const KEY_HOLD_TO_TALK: &str = "dictation.hold_to_talk";
const KEY_INPUT_DEVICE: &str = "dictation.input_device";
const KEY_PREROLL: &str = "dictation.preroll";
const KEY_POST_PROCESS_MIN_CHARS: &str = "dictation.post_process_min_chars";
const KEY_LANGUAGE: &str = "dictation.language";
const KEY_VOICE_COMMANDS_ENABLED: &str = "dictation.voice_commands_enabled";
const KEY_VOICE_COMMANDS: &str = "dictation.voice_commands";

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
    /// Shortest transcript worth sending to the LLM for cleanup.
    ///
    /// The cleanup pass costs a round trip — measured at roughly five seconds
    /// against a hosted model — and a short utterance has almost nothing for it
    /// to fix: "yes", "on my way", "ship it" arrive from the recogniser already
    /// correct. Paying five seconds to tidy nine characters is the wrong trade,
    /// and it is the one the feature made on every single run before this.
    ///
    /// It also closes a worse failure. A model handed a silent or near-silent
    /// clip does not return nothing, it returns something plausible: a real run
    /// transcribed `chars=0` and post-processed it into 140 characters the user
    /// never said. A floor means an empty transcript is never sent at all.
    #[serde(default = "post_process_min_chars_default")]
    pub post_process_min_chars: u32,
    /// BCP-47 tag to decode as, or `None` to let the model detect it.
    ///
    /// **Whisper only.** The ONNX transducer has no language setting, and the
    /// design review closed a defect where one was accepted from this setting
    /// and silently dropped on the way down (see the comment on
    /// `kea_engines::stt::parakeet`). So this is plumbed to exactly one engine,
    /// and the UI gates the control on the bound engine rather than on the
    /// model alone — a control that looks honoured and is not is worse than no
    /// control.
    ///
    /// `None` is not a missing value to paper over: it is auto-detect, which is
    /// what whisper does when `set_language` is never called.
    #[serde(default)]
    pub language: Option<String>,
}

/// A config written before the preroll existed has no row, and a payload from
/// an older frontend has no field. Both mean "the default", which is on.
fn preroll_default() -> bool {
    true
}

/// 100 characters — roughly a sentence and a half.
///
/// Below it a transcript is a phrase, and phrases come back clean; above it the
/// user is dictating prose, which is where the cleanup earns its round trip.
/// Configurable because the right answer depends on how someone dictates, and
/// `0` restores the old always-clean-up behaviour for anyone who wants it.
fn post_process_min_chars_default() -> u32 {
    100
}

/// The two standalone keys behind the voice-command pass.
///
/// Deliberately *not* fields on [`DictationSettings`]. That struct crosses the
/// Tauri boundary as one payload written whole, so a field added to it has to
/// be added to every literal that builds it, in a crate this module does not
/// own — and a settings blob is exactly the shape the one-row-per-key store
/// exists to avoid. These two are read on their own, next to each other,
/// because either alone is half a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceCommandSettings {
    /// The master switch.
    pub enabled: bool,
    /// The ids of the commands that may fire.
    ///
    /// `None` means the key has never been written, which takes the per-family
    /// defaults. `Some(vec![])` means the user switched everything off, which
    /// is a different thing and must survive a reread.
    pub enabled_ids: Option<Vec<String>>,
}

impl Default for VoiceCommandSettings {
    /// Off. A pass that rewrites what the user said starts from "not until
    /// something says so", including when a read fails.
    fn default() -> Self {
        Self {
            enabled: false,
            enabled_ids: None,
        }
    }
}

/// Reads a boolean that may have been written in either encoding.
///
/// The generic `set_setting` command takes a `String` and JSON-encodes it, so
/// a toggle saved from the UI lands as the JSON string `"true"` while a typed
/// caller writes a JSON bool. A reader that assumes one shape does not fail
/// loudly — it fails to deserialize, falls back to its default, and the toggle
/// is silently inert. That is how two app-context capture flags shipped dead,
/// and it is why this accepts both. (`src-tauri`'s `bool_setting` is the same
/// rule at the app layer.)
fn lenient_bool(value: Option<&serde_json::Value>, default: bool) -> bool {
    match value {
        Some(serde_json::Value::Bool(v)) => *v,
        Some(serde_json::Value::String(s)) => match s.as_str() {
            "true" => true,
            "false" => false,
            _ => default,
        },
        Some(other) => {
            tracing::warn!(value = %other, "unexpected boolean setting shape, using the default");
            default
        }
        None => default,
    }
}

/// The same trap one level up: a JSON array, or a JSON string holding one,
/// because the UI's only writer stringifies before `set_setting` stringifies
/// again.
///
/// A value that is neither is reported as `None` — "never configured" — rather
/// than as an empty list, so a corrupt row falls back to the defaults instead
/// of silently disabling every command.
fn lenient_string_list(value: Option<&serde_json::Value>) -> Option<Vec<String>> {
    match value? {
        serde_json::Value::Array(_) => serde_json::from_value(value?.clone()).ok(),
        serde_json::Value::String(s) => serde_json::from_str(s).ok(),
        // What `set` writes for a `None`: never configured, said out loud.
        serde_json::Value::Null => None,
        other => {
            tracing::warn!(value = %other, "unexpected list setting shape, using the defaults");
            None
        }
    }
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
            post_process_min_chars: self
                .settings
                .get(KEY_POST_PROCESS_MIN_CHARS)
                .await?
                .unwrap_or_else(post_process_min_chars_default),
            language: self.settings.get_optional(KEY_LANGUAGE).await?,
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
        self.settings.set(KEY_LANGUAGE, &cfg.language).await?;
        self.settings
            .set(KEY_POST_PROCESS_MIN_CHARS, &cfg.post_process_min_chars)
            .await?;
        self.settings.set(KEY_PREROLL, &cfg.preroll).await?;
        Ok(())
    }

    /// The voice-command pass's two keys.
    ///
    /// Read together and never from `get`, so a caller cannot end up with the
    /// master switch from one moment and the command set from another.
    pub async fn voice_commands(&self) -> Result<VoiceCommandSettings, KeaError> {
        let enabled = self
            .settings
            .get::<serde_json::Value>(KEY_VOICE_COMMANDS_ENABLED)
            .await?;
        let ids = self
            .settings
            .get::<serde_json::Value>(KEY_VOICE_COMMANDS)
            .await?;
        Ok(VoiceCommandSettings {
            enabled: lenient_bool(enabled.as_ref(), false),
            enabled_ids: lenient_string_list(ids.as_ref()),
        })
    }

    /// Writes both keys in the typed encoding (a JSON bool, a JSON array).
    ///
    /// The UI writes the stringified forms through the generic `set_setting`
    /// command instead; [`lenient_bool`] and [`lenient_string_list`] are what
    /// let the two writers coexist.
    pub async fn set_voice_commands(&self, cfg: &VoiceCommandSettings) -> Result<(), KeaError> {
        self.settings
            .set(KEY_VOICE_COMMANDS_ENABLED, &cfg.enabled)
            .await?;
        self.settings
            .set(KEY_VOICE_COMMANDS, &cfg.enabled_ids)
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
            input_device: Some("Yeti".into()),
            preroll: false,
            language: None,
            post_process_min_chars: 100,
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
            language: None,
            post_process_min_chars: 100,
        })
        .await
        .unwrap();
        repo.set(&DictationSettings {
            post_process: true,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: true,
            language: None,
            post_process_min_chars: 100,
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
            language: None,
            post_process_min_chars: 100,
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
            language: None,
            post_process_min_chars: 100,
        })
        .await
        .unwrap();
        assert!(!repo.get().await.unwrap().preroll);
    }

    /// Both writers. The UI stringifies everything through the generic
    /// `set_setting` command; a typed caller writes real JSON. A reader that
    /// only understood one of them would leave the settings page's toggles
    /// inert without saying anything, which is the defect this whole pair of
    /// helpers exists to prevent.
    #[tokio::test]
    async fn voice_command_settings_read_both_encodings() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool.clone());
        let repo = DictationSettingsRepo::new(SettingsRepo::new(pool));

        // Nothing written at all: off, and "never configured".
        let got = repo.voice_commands().await.unwrap();
        assert_eq!(got, VoiceCommandSettings::default());

        // Exactly what the settings page writes.
        settings
            .set("dictation.voice_commands_enabled", &"true".to_string())
            .await
            .unwrap();
        settings
            .set(
                "dictation.voice_commands",
                &r#"["period","comma"]"#.to_string(),
            )
            .await
            .unwrap();
        let got = repo.voice_commands().await.unwrap();
        assert!(got.enabled);
        assert_eq!(
            got.enabled_ids,
            Some(vec!["period".to_string(), "comma".to_string()])
        );

        // And what a typed caller writes.
        repo.set_voice_commands(&VoiceCommandSettings {
            enabled: true,
            enabled_ids: Some(vec!["scratch_that".to_string()]),
        })
        .await
        .unwrap();
        let got = repo.voice_commands().await.unwrap();
        assert!(got.enabled);
        assert_eq!(got.enabled_ids, Some(vec!["scratch_that".to_string()]));
    }

    /// An empty list is a decision — "every command off" — and must not read
    /// back as "never configured", which would turn the defaults back on.
    #[tokio::test]
    async fn an_empty_command_list_survives_a_reread() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = DictationSettingsRepo::new(SettingsRepo::new(pool));
        repo.set_voice_commands(&VoiceCommandSettings {
            enabled: true,
            enabled_ids: Some(Vec::new()),
        })
        .await
        .unwrap();
        assert_eq!(
            repo.voice_commands().await.unwrap().enabled_ids,
            Some(Vec::new())
        );

        // Whereas a cleared key — the JSON literal `null` — is not.
        repo.set_voice_commands(&VoiceCommandSettings {
            enabled: false,
            enabled_ids: None,
        })
        .await
        .unwrap();
        assert_eq!(repo.voice_commands().await.unwrap().enabled_ids, None);
    }

    /// A garbled row falls back to the defaults rather than to "everything
    /// off": a setting nobody can read is not a setting the user chose.
    #[test]
    fn an_unreadable_row_reads_as_never_configured() {
        use serde_json::json;
        assert_eq!(lenient_string_list(Some(&json!(42))), None);
        assert_eq!(lenient_string_list(Some(&json!("not json"))), None);
        assert_eq!(lenient_string_list(None), None);
        assert!(!lenient_bool(Some(&json!("yes")), false));
        assert!(lenient_bool(None, true));
        assert!(lenient_bool(Some(&json!(true)), false));
        assert!(lenient_bool(Some(&json!("true")), false));
        assert!(!lenient_bool(Some(&json!("false")), true));
    }
}
