//! Migration 0010 (presets -> custom styles), run against databases seeded the
//! way real ones look: migrated up to 0009, with preset data in them.

use std::borrow::Cow;

use kea_core::rewrite::{RewriteMode, StyleRef, StyleRepo};
use kea_core::store::db::{open_pool, run_config_migrations};
use sqlx::migrate::Migrator;
use sqlx::SqlitePool;

static ALL: Migrator = sqlx::migrate!("./migrations/config");

/// The schema as it was before custom styles: every migration below 0010.
async fn pre_styles_pool() -> SqlitePool {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    let before: Vec<_> = ALL
        .migrations
        .iter()
        .filter(|m| m.version < 10)
        .cloned()
        .collect();
    let old = Migrator {
        migrations: Cow::Owned(before),
        ..Migrator::DEFAULT
    };
    old.run(&pool).await.unwrap();
    pool
}

async fn setting(pool: &SqlitePool, key: &str) -> Option<String> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn put_setting(pool: &SqlitePool, key: &str, json: &str) {
    sqlx::query("INSERT INTO settings(key, value) VALUES(?, ?)")
        .bind(key)
        .bind(json)
        .execute(pool)
        .await
        .unwrap();
}

async fn put_preset(pool: &SqlitePool, id: &str, name: &str, instruction: &str) {
    // Including the per-preset LLM columns from 0008, which are dropped.
    sqlx::query(
        "INSERT INTO rewrite_presets(id, name, instruction, llm_engine_id, llm_model) \
         VALUES(?, ?, ?, 'openai', 'gpt-5')",
    )
    .bind(id)
    .bind(name)
    .bind(instruction)
    .execute(pool)
    .await
    .unwrap();
}

async fn table_exists(pool: &SqlitePool, name: &str) -> bool {
    let n: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
    n > 0
}

const LONG: &str = "You turn rough notes into a clear, well-structured prompt for an AI.\n\nKeep the user's intent.";

/// The owner's real database: a long "prompt improvement" preset, a junk
/// "None" preset that is the active one, active_mode "improve", and an app
/// profile that forces the real preset.
#[tokio::test]
async fn owners_database_keeps_one_style_and_stays_on_improve() {
    let pool = pre_styles_pool().await;
    put_preset(&pool, "preset-1", "prompt improvement", LONG).await;
    put_preset(&pool, "preset-2", "None", "None").await;
    put_setting(&pool, "rewrite.active_preset_id", "\"preset-2\"").await;
    put_setting(&pool, "rewrite.active_mode", "\"improve\"").await;
    put_setting(&pool, "rewrite.custom_instruction", "\"\"").await;
    sqlx::query(
        "INSERT INTO app_profiles(id, name, preset_id) VALUES('profile-1', 'Notes', 'preset-1')",
    )
    .execute(&pool)
    .await
    .unwrap();

    run_config_migrations(&pool).await.unwrap();

    let styles = StyleRepo::new(pool.clone()).list().await.unwrap();
    assert_eq!(styles.len(), 1);
    assert_eq!(styles[0].id, "preset-1");
    assert_eq!(styles[0].name, "prompt improvement");
    assert_eq!(styles[0].instruction, LONG);

    // The active preset was the junk one, so the active style is still Improve.
    assert_eq!(
        setting(&pool, "rewrite.active_mode").await.as_deref(),
        Some("\"improve\"")
    );
    assert_eq!(
        StyleRepo::new(pool.clone()).active().await.unwrap(),
        StyleRef::Builtin(RewriteMode::Improve)
    );

    // Nothing of the preset feature is left.
    assert!(!table_exists(&pool, "rewrite_presets").await);
    assert_eq!(setting(&pool, "rewrite.active_preset_id").await, None);
    // Unrelated settings are untouched.
    assert_eq!(
        setting(&pool, "rewrite.custom_instruction")
            .await
            .as_deref(),
        Some("\"\"")
    );

    // The profile that forced the real preset now forces that style.
    let forced: Option<String> =
        sqlx::query_scalar("SELECT rewrite_style FROM app_profiles WHERE id = 'profile-1'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(forced.as_deref(), Some("preset-1"));
}

#[tokio::test]
async fn an_active_preset_that_migrated_becomes_the_active_style() {
    let pool = pre_styles_pool().await;
    put_preset(&pool, "p1", "Formal", "Be formal").await;
    put_setting(&pool, "rewrite.active_preset_id", "\"p1\"").await;
    put_setting(&pool, "rewrite.active_mode", "\"concise\"").await;

    run_config_migrations(&pool).await.unwrap();

    assert_eq!(
        setting(&pool, "rewrite.active_mode").await.as_deref(),
        Some("\"p1\"")
    );
    assert_eq!(
        StyleRepo::new(pool.clone()).active().await.unwrap(),
        StyleRef::Custom("p1".into())
    );
    assert_eq!(setting(&pool, "rewrite.active_preset_id").await, None);
}

#[tokio::test]
async fn an_active_preset_that_is_missing_keeps_the_active_mode() {
    let pool = pre_styles_pool().await;
    put_preset(&pool, "p1", "Formal", "Be formal").await;
    put_setting(&pool, "rewrite.active_preset_id", "\"deleted-long-ago\"").await;
    put_setting(&pool, "rewrite.active_mode", "\"friendly\"").await;

    run_config_migrations(&pool).await.unwrap();

    assert_eq!(
        setting(&pool, "rewrite.active_mode").await.as_deref(),
        Some("\"friendly\"")
    );
    assert_eq!(setting(&pool, "rewrite.active_preset_id").await, None);
}

#[tokio::test]
async fn an_active_preset_with_no_active_mode_row_sets_one() {
    let pool = pre_styles_pool().await;
    put_preset(&pool, "p1", "Formal", "Be formal").await;
    put_setting(&pool, "rewrite.active_preset_id", "\"p1\"").await;

    run_config_migrations(&pool).await.unwrap();

    assert_eq!(
        setting(&pool, "rewrite.active_mode").await.as_deref(),
        Some("\"p1\"")
    );
}

#[tokio::test]
async fn only_an_instruction_of_exactly_none_is_junk() {
    let pool = pre_styles_pool().await;
    put_preset(&pool, "a", "None", "None").await;
    put_preset(&pool, "b", "lower", "  none \n").await;
    put_preset(&pool, "c", "upper", "NONE").await;
    put_preset(&pool, "d", "Real", "None of the above, but be formal").await;
    put_preset(&pool, "e", "Empty", "").await;

    run_config_migrations(&pool).await.unwrap();

    let mut ids: Vec<_> = StyleRepo::new(pool)
        .list()
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.id)
        .collect();
    ids.sort();
    assert_eq!(ids, vec!["d", "e"]);
}

#[tokio::test]
async fn profiles_keep_their_mode_and_a_skipped_preset_falls_back_to_it() {
    let pool = pre_styles_pool().await;
    put_preset(&pool, "junk", "None", "None").await;
    put_preset(&pool, "real", "Real", "Be formal").await;
    for (id, mode, preset) in [
        ("mode-only", Some("friendly"), None),
        ("junk-preset", Some("concise"), Some("junk")),
        ("junk-no-mode", None, Some("junk")),
        // The preset used to win over the mode, and still does.
        ("both", Some("concise"), Some("real")),
        ("neither", None, None),
    ] {
        sqlx::query(
            "INSERT INTO app_profiles(id, name, match_bundle_id, rewrite_mode, preset_id, \
             llm_engine_id, post_process, insertion_mode, priority) \
             VALUES(?, ?, 'com.example', ?, ?, 'local', 1, 'paste', 4)",
        )
        .bind(id)
        .bind(id)
        .bind(mode)
        .bind(preset)
        .execute(&pool)
        .await
        .unwrap();
    }

    run_config_migrations(&pool).await.unwrap();

    let rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT id, rewrite_style FROM app_profiles ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let get = |id: &str| rows.iter().find(|r| r.0 == id).unwrap().1.clone();
    assert_eq!(get("mode-only").as_deref(), Some("friendly"));
    assert_eq!(get("junk-preset").as_deref(), Some("concise"));
    assert_eq!(get("junk-no-mode"), None);
    assert_eq!(get("both").as_deref(), Some("real"));
    assert_eq!(get("neither"), None);

    // The rebuild kept every other column and the lookup index.
    let (engine, post, insertion, priority): (String, i64, String, i64) = sqlx::query_as(
        "SELECT llm_engine_id, post_process, insertion_mode, priority \
         FROM app_profiles WHERE id = 'both'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (engine.as_str(), post, insertion.as_str(), priority),
        ("local", 1, "paste", 4)
    );
    assert!(table_exists(&pool, "app_profiles").await);
    let idx: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'idx_app_profiles_lookup'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(idx, 1);
}

#[tokio::test]
async fn a_fresh_install_has_styles_and_no_presets() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    run_config_migrations(&pool).await.unwrap();
    assert!(table_exists(&pool, "rewrite_styles").await);
    assert!(!table_exists(&pool, "rewrite_presets").await);
    assert!(StyleRepo::new(pool).list().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_hand_edited_active_preset_value_does_not_fail_the_migration() {
    let pool = pre_styles_pool().await;
    put_preset(&pool, "p1", "Formal", "Be formal").await;
    put_setting(&pool, "rewrite.active_preset_id", "not json at all").await;
    put_setting(&pool, "rewrite.active_mode", "\"friendly\"").await;

    run_config_migrations(&pool).await.unwrap();

    assert_eq!(
        setting(&pool, "rewrite.active_mode").await.as_deref(),
        Some("\"friendly\"")
    );
    assert_eq!(setting(&pool, "rewrite.active_preset_id").await, None);
}
