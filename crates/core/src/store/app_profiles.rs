//! Storage for per-app profiles (`app_profiles`, config pool).

use sqlx::SqlitePool;

use crate::app_context::AppProfile;
use crate::error::KeaError;

/// Every column, in one place, so the four queries below cannot drift apart.
/// `SELECT *` is avoided deliberately: `FromRow` matches by name, and a future
/// migration adding a column would otherwise change what each of these reads.
const COLUMNS: &str = "id, name, enabled, priority, match_bundle_id, match_url_glob, \
                       rewrite_mode, preset_id, llm_engine_id, llm_model, llm_provider_ref, \
                       post_process, insertion_mode, created_at";

pub struct AppProfileRepo {
    pool: SqlitePool,
}

impl AppProfileRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn upsert(&self, p: &AppProfile) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO app_profiles(id, name, enabled, priority, match_bundle_id, \
                 match_url_glob, rewrite_mode, preset_id, llm_engine_id, llm_model, \
                 llm_provider_ref, post_process, insertion_mode, created_at)
             VALUES(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, COALESCE(NULLIF(?, ''), datetime('now')))
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 enabled = excluded.enabled,
                 priority = excluded.priority,
                 match_bundle_id = excluded.match_bundle_id,
                 match_url_glob = excluded.match_url_glob,
                 rewrite_mode = excluded.rewrite_mode,
                 preset_id = excluded.preset_id,
                 llm_engine_id = excluded.llm_engine_id,
                 llm_model = excluded.llm_model,
                 llm_provider_ref = excluded.llm_provider_ref,
                 post_process = excluded.post_process,
                 insertion_mode = excluded.insertion_mode",
        )
        .bind(&p.id)
        .bind(&p.name)
        .bind(p.enabled)
        .bind(p.priority)
        .bind(&p.match_bundle_id)
        .bind(&p.match_url_glob)
        .bind(&p.rewrite_mode)
        .bind(&p.preset_id)
        .bind(&p.llm_engine_id)
        .bind(&p.llm_model)
        .bind(&p.llm_provider_ref)
        .bind(p.post_process)
        .bind(&p.insertion_mode)
        // `created_at` is stamped by SQLite on first insert and never
        // overwritten, so a round-tripped row keeps its original timestamp
        // even though the struct carries it.
        .bind(&p.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every profile, in the order the settings table shows them.
    ///
    /// The ORDER BY is for the UI only — `resolve_profile` re-derives its own
    /// total order and does not depend on this one.
    pub async fn list(&self) -> Result<Vec<AppProfile>, KeaError> {
        let rows = sqlx::query_as::<_, AppProfile>(&format!(
            "SELECT {COLUMNS} FROM app_profiles ORDER BY priority DESC, name COLLATE NOCASE"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// The rules the matcher should consider. The hotkey path only ever wants
    /// the active ones, and letting every caller re-filter is how one of them
    /// eventually forgets to.
    pub async fn list_enabled(&self) -> Result<Vec<AppProfile>, KeaError> {
        let rows = sqlx::query_as::<_, AppProfile>(&format!(
            "SELECT {COLUMNS} FROM app_profiles WHERE enabled = 1 \
             ORDER BY priority DESC, name COLLATE NOCASE"
        ))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn get(&self, id: &str) -> Result<Option<AppProfile>, KeaError> {
        let row = sqlx::query_as::<_, AppProfile>(&format!(
            "SELECT {COLUMNS} FROM app_profiles WHERE id = ?"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn delete(&self, id: &str) -> Result<(), KeaError> {
        sqlx::query("DELETE FROM app_profiles WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_context::{resolve_profile, InsertionMode, ProfileQuery};
    use crate::rewrite::RewriteMode;
    use crate::rewrite::{PresetRepo, RewritePreset};
    use crate::store::db::{open_pool, run_config_migrations};

    async fn repo() -> (AppProfileRepo, SqlitePool) {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        (AppProfileRepo::new(pool.clone()), pool)
    }

    fn slack() -> AppProfile {
        let mut p = AppProfile::new("slack", "Slack");
        p.match_bundle_id = Some("com.tinyspeck.slackmacgap".into());
        p.rewrite_mode = Some(RewriteMode::Friendly.as_str().into());
        p.post_process = Some(true);
        p.insertion_mode = Some(InsertionMode::ClipboardPaste.as_str().into());
        p
    }

    #[tokio::test]
    async fn profile_roundtrips_with_every_override_set() {
        let (repo, _pool) = repo().await;
        let mut p = slack();
        p.priority = 7;
        p.match_url_glob = Some("*.slack.com/*".into());
        p.llm_engine_id = Some("openai".into());
        p.llm_model = Some("gpt-4o".into());
        p.llm_provider_ref = Some("work".into());
        repo.upsert(&p).await.unwrap();

        let got = repo.get("slack").await.unwrap().unwrap();
        assert_eq!(got.mode(), Some(RewriteMode::Friendly));
        assert_eq!(got.insertion(), Some(InsertionMode::ClipboardPaste));
        assert_eq!(got.post_process, Some(true));
        assert_eq!(got.llm_binding().unwrap().engine_id, "openai");
        assert!(!got.created_at.is_empty(), "created_at must be stamped");

        repo.delete("slack").await.unwrap();
        assert_eq!(repo.get("slack").await.unwrap(), None);
    }

    #[tokio::test]
    async fn post_process_is_a_tri_state_across_the_db() {
        let (repo, _pool) = repo().await;
        for (id, want) in [("on", Some(true)), ("off", Some(false)), ("inherit", None)] {
            let mut p = AppProfile::new(id, id);
            p.post_process = want;
            repo.upsert(&p).await.unwrap();
            // The whole point of the nullable INTEGER: "force off" and
            // "inherit" must not collapse into each other on a round trip.
            assert_eq!(
                repo.get(id).await.unwrap().unwrap().post_process,
                want,
                "{id}"
            );
        }
    }

    #[tokio::test]
    async fn upsert_replaces_an_existing_id_and_keeps_created_at() {
        let (repo, _pool) = repo().await;
        repo.upsert(&slack()).await.unwrap();
        let first = repo.get("slack").await.unwrap().unwrap();

        let mut edited = first.clone();
        edited.name = "Slack (work)".into();
        edited.post_process = Some(false);
        repo.upsert(&edited).await.unwrap();

        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].name, "Slack (work)");
        assert_eq!(all[0].post_process, Some(false));
        assert_eq!(all[0].created_at, first.created_at);
    }

    #[tokio::test]
    async fn list_enabled_skips_disabled_rows() {
        let (repo, _pool) = repo().await;
        repo.upsert(&slack()).await.unwrap();
        let mut off = AppProfile::new("term", "Terminal");
        off.enabled = false;
        repo.upsert(&off).await.unwrap();

        assert_eq!(repo.list().await.unwrap().len(), 2);
        let enabled = repo.list_enabled().await.unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].id, "slack");
    }

    /// The foreign key is the reason a deleted preset cannot leave a profile
    /// pointing at an id `build_llm_request` would later fail on.
    #[tokio::test]
    async fn deleting_a_preset_nulls_the_profile_reference() {
        let (repo, pool) = repo().await;
        let presets = PresetRepo::new(pool.clone());
        presets
            .upsert(&RewritePreset::new("p1", "Terse", "Be terse."))
            .await
            .unwrap();

        let mut p = slack();
        p.preset_id = Some("p1".into());
        repo.upsert(&p).await.unwrap();

        presets.delete("p1").await.unwrap();
        assert_eq!(repo.get("slack").await.unwrap().unwrap().preset_id, None);
    }

    /// The repo and the matcher meet here: rows out of SQLite must feed
    /// `resolve_profile` unchanged.
    #[tokio::test]
    async fn stored_rows_resolve_end_to_end() {
        let (repo, _pool) = repo().await;
        repo.upsert(&slack()).await.unwrap();
        let mut catch_all = AppProfile::new("default", "Everything else");
        catch_all.priority = 99;
        repo.upsert(&catch_all).await.unwrap();

        let rows = repo.list_enabled().await.unwrap();
        let hit =
            resolve_profile(ProfileQuery::bundle("com.tinyspeck.slackmacgap"), &rows).unwrap();
        assert_eq!(hit.id, "slack");
        let miss = resolve_profile(ProfileQuery::bundle("com.apple.Terminal"), &rows).unwrap();
        assert_eq!(miss.id, "default");
    }
}
