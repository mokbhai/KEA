//! The prompt palette's instruction history — the list arrow-up walks.
//!
//! # Why not `conversations`
//!
//! [`crate::store::conversations::ConversationRepo`] is the wrong home on four
//! counts, all visible in that file: it lives on the **data** pool while a list
//! of the user's own saved phrases is configuration, like presets; its rows are
//! keyed to an `action_id` and an `engine_id`, neither of which a dismissed or
//! cancelled palette run has; it is gated by `history.store_conversations` and
//! pruned at 90 days, and "stop keeping my text" should not silently mean "lose
//! my shortcuts"; and `list_messages` returns the *rendered prompt*, so
//! recovering the bare instruction would mean parsing it back out of the Ask
//! KEA template — which breaks the moment a prompt override is in play.
//!
//! # Why a settings row rather than a table
//!
//! The list is at most [`HISTORY_LIMIT`] short strings, read whole and written
//! whole, and it is never joined against anything. [`ProviderConfigRepo`]
//! already stores structured config this way in this same directory, so this
//! needs no migration and no second pool handle. Should it ever want a
//! `use_count` or a full-text search, the repo below is the only thing that
//! would change — no caller learns how it is stored.
//!
//! [`ProviderConfigRepo`]: crate::rewrite::provider::ProviderConfigRepo

use crate::error::KeaError;
use crate::store::settings::SettingsRepo;

/// How many distinct instructions are kept. Arrow-up is a shortcut, not an
/// archive: past a few dozen presses it is faster to retype.
pub const HISTORY_LIMIT: usize = 50;

/// Settings key holding the whole list, newest first.
pub const HISTORY_KEY: &str = "palette.history";

/// Whether instructions are remembered at all. Instructions are content too
/// ("rewrite this rejection letter for Bob"), so this is a real switch and not
/// a convenience.
pub const STORE_HISTORY_SETTING: &str = "palette.store_history";

pub struct PaletteHistoryRepo {
    settings: SettingsRepo,
}

impl PaletteHistoryRepo {
    pub fn new(settings: SettingsRepo) -> Self {
        Self { settings }
    }

    /// The most recently used instructions, newest first.
    pub async fn recent(&self, limit: usize) -> Result<Vec<String>, KeaError> {
        let mut all = self.load().await?;
        all.truncate(limit);
        Ok(all)
    }

    /// Records one use: moves `instruction` to the front, or inserts it.
    ///
    /// Re-running "make this shorter" must not append a duplicate — arrow-up
    /// walking ten copies of the last instruction is the failure this exists
    /// to prevent — so an existing entry is moved rather than added.
    pub async fn record(&self, instruction: &str) -> Result<(), KeaError> {
        let instruction = instruction.trim();
        if instruction.is_empty() {
            return Ok(());
        }
        let mut all = self.load().await?;
        all.retain(|existing| existing != instruction);
        all.insert(0, instruction.to_string());
        all.truncate(HISTORY_LIMIT);
        self.settings.set(HISTORY_KEY, &all).await
    }

    pub async fn clear(&self) -> Result<(), KeaError> {
        self.settings.set(HISTORY_KEY, &Vec::<String>::new()).await
    }

    /// Through `get_optional`: [`Self::clear`] and a stored `null` both have to
    /// read back as "nothing yet" rather than as a deserialize error that would
    /// then be permanent.
    async fn load(&self) -> Result<Vec<String>, KeaError> {
        Ok(self
            .settings
            .get_optional::<Vec<String>>(HISTORY_KEY)
            .await?
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    async fn repo() -> PaletteHistoryRepo {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        PaletteHistoryRepo::new(SettingsRepo::new(pool))
    }

    #[tokio::test]
    async fn records_and_reads_back_newest_first() {
        let repo = repo().await;
        assert!(repo.recent(10).await.unwrap().is_empty());

        repo.record("make it shorter").await.unwrap();
        repo.record("translate to German").await.unwrap();

        assert_eq!(
            repo.recent(10).await.unwrap(),
            vec!["translate to German", "make it shorter"]
        );
    }

    #[tokio::test]
    async fn the_same_instruction_twice_stays_one_entry() {
        let repo = repo().await;
        repo.record("make it shorter").await.unwrap();
        repo.record("fix the grammar").await.unwrap();
        repo.record("make it shorter").await.unwrap();

        // One entry, and back at the front: arrow-up must not walk two copies.
        assert_eq!(
            repo.recent(10).await.unwrap(),
            vec!["make it shorter", "fix the grammar"]
        );
    }

    #[tokio::test]
    async fn trims_past_the_limit() {
        let repo = repo().await;
        for i in 0..HISTORY_LIMIT + 5 {
            repo.record(&format!("instruction {i}")).await.unwrap();
        }
        let all = repo.recent(1000).await.unwrap();
        assert_eq!(all.len(), HISTORY_LIMIT);
        assert_eq!(all[0], format!("instruction {}", HISTORY_LIMIT + 4));
        // The oldest five are gone, not merely hidden behind a limit.
        assert!(!all.contains(&"instruction 0".to_string()));
    }

    #[tokio::test]
    async fn blank_instructions_are_not_recorded() {
        let repo = repo().await;
        repo.record("   \n ").await.unwrap();
        repo.record("").await.unwrap();
        assert!(repo.recent(10).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn instructions_are_stored_trimmed() {
        // Otherwise "  shorten" and "shorten" are two entries that look alike.
        let repo = repo().await;
        repo.record("  shorten this  ").await.unwrap();
        repo.record("shorten this").await.unwrap();
        assert_eq!(repo.recent(10).await.unwrap(), vec!["shorten this"]);
    }

    #[tokio::test]
    async fn clear_empties_the_list_and_stays_readable() {
        let repo = repo().await;
        repo.record("one").await.unwrap();
        repo.clear().await.unwrap();
        assert!(repo.recent(10).await.unwrap().is_empty());
        // Still writable afterwards — a cleared list is not a broken one.
        repo.record("two").await.unwrap();
        assert_eq!(repo.recent(10).await.unwrap(), vec!["two"]);
    }

    #[tokio::test]
    async fn recent_honours_its_limit() {
        let repo = repo().await;
        for i in 0..5 {
            repo.record(&format!("i{i}")).await.unwrap();
        }
        assert_eq!(repo.recent(2).await.unwrap(), vec!["i4", "i3"]);
    }
}
