use serde::{Deserialize, Serialize};

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

const KEY_SPEAK_ANSWERS: &str = "assistant.speak_answers";
const KEY_SHOW_ANSWERS: &str = "assistant.show_answers";

/// Both answer channels are on unless the user turns one off. Named rather
/// than inlined so the reader, the serde default and the `Default` impl cannot
/// drift apart — the way a `false` in any one of them would quietly ship an
/// assistant that answers into the void.
const DEFAULT_ON: bool = true;

/// The two output switches for an assistant answer.
///
/// Deliberately *not* holding the LLM and TTS bindings. A binding lives in the
/// `bindings` table and is resolved by `SlotResolver`, so duplicating it here
/// would give the same decision two homes and a way to disagree — the binding
/// the settings page wrote versus the binding the session actually resolved.
/// The alternative, folding the bindings in so one payload carried everything
/// the assistant page edits, would have cost exactly that: a second source of
/// truth for which engine runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssistantSettings {
    /// Speak the answer aloud through the resolved TTS slot.
    ///
    /// On by default. The whole point of a voice assistant is that the answer
    /// arrives without looking at the screen, so silence has to be something
    /// the user asked for.
    #[serde(default = "default_on")]
    pub speak_answers: bool,
    /// Display the answer text in the session surface.
    ///
    /// On by default, and independent of [`speak_answers`](Self::speak_answers):
    /// the spec makes each channel switchable on its own, including the
    /// hands-free case where an answer is spoken and never shown.
    #[serde(default = "default_on")]
    pub show_answers: bool,
}

/// A payload from an older frontend has no field for either switch, and a
/// config written before they existed has no row. Both mean "the default".
fn default_on() -> bool {
    DEFAULT_ON
}

impl Default for AssistantSettings {
    fn default() -> Self {
        Self {
            speak_answers: DEFAULT_ON,
            show_answers: DEFAULT_ON,
        }
    }
}

pub struct AssistantSettingsRepo {
    settings: SettingsRepo,
}

impl AssistantSettingsRepo {
    pub fn new(settings: SettingsRepo) -> Self {
        Self { settings }
    }

    /// One row per key, absent meaning the default.
    ///
    /// That is the entire migration story for every install that predates the
    /// assistant: a serialized blob would have had to be versioned and
    /// re-parsed, and a blob missing a field fails the whole read rather than
    /// one setting. These keys are only ever written through
    /// [`set`](Self::set), which encodes a JSON bool, so the lenient decoding
    /// `DictationSettingsRepo::voice_commands` needs — it shares its keys with
    /// the stringifying generic `set_setting` command — has no reason to exist
    /// here. Point a generic writer at these keys and that changes.
    pub async fn get(&self) -> Result<AssistantSettings, KeaError> {
        Ok(AssistantSettings {
            speak_answers: self
                .settings
                .get(KEY_SPEAK_ANSWERS)
                .await?
                .unwrap_or(DEFAULT_ON),
            show_answers: self
                .settings
                .get(KEY_SHOW_ANSWERS)
                .await?
                .unwrap_or(DEFAULT_ON),
        })
    }

    pub async fn set(&self, cfg: &AssistantSettings) -> Result<(), KeaError> {
        self.settings
            .set(KEY_SPEAK_ANSWERS, &cfg.speak_answers)
            .await?;
        self.settings
            .set(KEY_SHOW_ANSWERS, &cfg.show_answers)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    async fn repo() -> AssistantSettingsRepo {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        AssistantSettingsRepo::new(SettingsRepo::new(pool))
    }

    #[tokio::test]
    async fn both_switches_survive_a_roundtrip() {
        let repo = repo().await;
        let cfg = AssistantSettings {
            speak_answers: false,
            show_answers: true,
        };
        repo.set(&cfg).await.unwrap();
        assert_eq!(repo.get().await.unwrap(), cfg);
    }

    /// The migration path for every existing install: no row has ever been
    /// written for either key, and both must read as enabled. A plain
    /// `bool::default()` here would be `false`, which is an assistant that
    /// neither speaks nor shows its answer — a feature that looks broken
    /// rather than one that is switched off.
    #[tokio::test]
    async fn an_install_that_predates_these_settings_both_speaks_and_shows() {
        let repo = repo().await;
        let got = repo.get().await.unwrap();
        assert!(got.speak_answers, "an absent row means speaking is on");
        assert!(got.show_answers, "an absent row means showing is on");
        assert_eq!(got, AssistantSettings::default());
    }

    /// Each key defaults on independently, so a user who wrote only one of
    /// them must not have the other decided for them. Written through the
    /// bare `SettingsRepo` because that is what a build knowing only one key
    /// would have left behind.
    #[tokio::test]
    async fn writing_one_switch_leaves_the_other_at_its_default() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let settings = SettingsRepo::new(pool);
        settings
            .set("assistant.speak_answers", &false)
            .await
            .unwrap();

        let got = AssistantSettingsRepo::new(settings).get().await.unwrap();
        assert!(!got.speak_answers);
        assert!(got.show_answers, "the unwritten key keeps its default");
    }

    /// Both switches default on, so "off" is the value a read can lose: an
    /// `unwrap_or(true)` over a row that failed to write would silently turn
    /// the channel back on and the user would be told nothing.
    #[tokio::test]
    async fn turning_both_switches_off_sticks_across_a_reread() {
        let repo = repo().await;
        repo.set(&AssistantSettings {
            speak_answers: false,
            show_answers: false,
        })
        .await
        .unwrap();

        let got = repo.get().await.unwrap();
        assert!(!got.speak_answers);
        assert!(!got.show_answers);
    }

    /// The struct crosses the Tauri boundary, so a payload from a frontend
    /// that knows neither switch must arrive as "both on" rather than failing
    /// to deserialize and taking the whole settings save down with it.
    #[test]
    fn a_payload_missing_both_fields_deserializes_to_the_defaults() {
        let got: AssistantSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(got, AssistantSettings::default());
    }
}
