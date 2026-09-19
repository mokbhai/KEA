use serde::{Deserialize, Serialize};

use crate::error::KeaError;
use crate::meetings::interim::InterimCadence;
use crate::store::settings::SettingsRepo;

const KEY_SEGMENT_DURATION: &str = "meetings.segment_duration_secs";
const KEY_PREFER_SYSTEM_AUDIO: &str = "meetings.prefer_system_audio";
const KEY_INTERIM_NOTES: &str = "meetings.interim_notes";
const KEY_INTERIM_EVERY_SEGMENTS: &str = "meetings.interim_every_segments";
const KEY_INTERIM_EVERY_MINUTES: &str = "meetings.interim_every_minutes";
const KEY_CALENDAR_TITLES: &str = "meetings.calendar_titles";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingSettings {
    pub segment_duration_secs: u32,
    pub prefer_system_audio: bool,
    /// Write notes on a cadence during the meeting instead of only at stop.
    ///
    /// Off by default and `#[serde(default)]` on the way in: this spends the
    /// user's tokens on a schedule they did not press a button for, and a
    /// settings payload from a frontend that predates the field must not be
    /// read as "yes".
    #[serde(default)]
    pub interim_notes: bool,
    #[serde(default = "default_interim_every_segments")]
    pub interim_every_segments: u32,
    #[serde(default = "default_interim_every_minutes")]
    pub interim_every_minutes: u32,
    /// Name a meeting after the calendar event it happened during.
    ///
    /// Off by default for the same reason, plus one of its own: turning it on
    /// is what asks for calendar access, and prompting for that the first time
    /// somebody records a meeting would interrupt the flow this feature exists
    /// to improve.
    #[serde(default)]
    pub calendar_titles: bool,
}

fn default_interim_every_segments() -> u32 {
    InterimCadence::default().every_segments
}

fn default_interim_every_minutes() -> u32 {
    InterimCadence::default().every_minutes
}

impl Default for MeetingSettings {
    fn default() -> Self {
        let cadence = InterimCadence::default();
        Self {
            segment_duration_secs: 30,
            prefer_system_audio: true,
            interim_notes: false,
            interim_every_segments: cadence.every_segments,
            interim_every_minutes: cadence.every_minutes,
            calendar_titles: false,
        }
    }
}

impl MeetingSettings {
    /// The cadence these settings describe, with the per-meeting ceiling this
    /// crate owns rather than the user.
    pub fn interim_cadence(&self) -> InterimCadence {
        InterimCadence {
            every_segments: self.interim_every_segments,
            every_minutes: self.interim_every_minutes,
            ..InterimCadence::default()
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
        // One row per key, so an absent row reads as the default and no
        // migration is needed when a key is added here.
        let defaults = MeetingSettings::default();
        Ok(MeetingSettings {
            segment_duration_secs: self
                .settings
                .get(KEY_SEGMENT_DURATION)
                .await?
                .unwrap_or(defaults.segment_duration_secs),
            prefer_system_audio: self
                .settings
                .get(KEY_PREFER_SYSTEM_AUDIO)
                .await?
                .unwrap_or(defaults.prefer_system_audio),
            interim_notes: self
                .settings
                .get(KEY_INTERIM_NOTES)
                .await?
                .unwrap_or(defaults.interim_notes),
            interim_every_segments: self
                .settings
                .get(KEY_INTERIM_EVERY_SEGMENTS)
                .await?
                .unwrap_or(defaults.interim_every_segments),
            interim_every_minutes: self
                .settings
                .get(KEY_INTERIM_EVERY_MINUTES)
                .await?
                .unwrap_or(defaults.interim_every_minutes),
            calendar_titles: self
                .settings
                .get(KEY_CALENDAR_TITLES)
                .await?
                .unwrap_or(defaults.calendar_titles),
        })
    }

    pub async fn set(&self, cfg: &MeetingSettings) -> Result<(), KeaError> {
        self.settings
            .set(KEY_SEGMENT_DURATION, &cfg.segment_duration_secs)
            .await?;
        self.settings
            .set(KEY_PREFER_SYSTEM_AUDIO, &cfg.prefer_system_audio)
            .await?;
        self.settings
            .set(KEY_INTERIM_NOTES, &cfg.interim_notes)
            .await?;
        self.settings
            .set(KEY_INTERIM_EVERY_SEGMENTS, &cfg.interim_every_segments)
            .await?;
        self.settings
            .set(KEY_INTERIM_EVERY_MINUTES, &cfg.interim_every_minutes)
            .await?;
        self.settings
            .set(KEY_CALENDAR_TITLES, &cfg.calendar_titles)
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
            interim_notes: true,
            interim_every_segments: 12,
            interim_every_minutes: 7,
            calendar_titles: true,
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

    /// Both cost-spending features are opt-in. A default that was ever `true`
    /// would bill every user who never opened this page.
    #[test]
    fn the_cost_spending_features_default_off() {
        let cfg = MeetingSettings::default();
        assert!(!cfg.interim_notes);
        assert!(!cfg.calendar_titles);
    }

    /// A settings payload from a frontend that predates these fields must
    /// leave them off rather than fail to deserialize — the whole command
    /// would return an error and the user could not save anything at all.
    #[test]
    fn an_older_payload_still_deserializes() {
        let cfg: MeetingSettings =
            serde_json::from_str(r#"{"segment_duration_secs":30,"prefer_system_audio":true}"#)
                .unwrap();
        assert_eq!(cfg, MeetingSettings::default());
    }

    #[test]
    fn the_cadence_carries_the_users_two_numbers_and_keas_ceiling() {
        let cfg = MeetingSettings {
            interim_every_segments: 3,
            interim_every_minutes: 2,
            ..MeetingSettings::default()
        };
        let cadence = cfg.interim_cadence();
        assert_eq!(cadence.every_segments, 3);
        assert_eq!(cadence.every_minutes, 2);
        assert_eq!(cadence.max_passes, InterimCadence::default().max_passes);
    }
}
