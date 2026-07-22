use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;
use crate::error::KeaError;

pub async fn open_pool(url: &str) -> Result<SqlitePool, KeaError> {
    // Enable foreign-key enforcement explicitly on every pooled connection.
    // sqlx defaults it on, but the ON DELETE CASCADE rules on meeting/
    // conversation children are data-integrity-critical, so we don't rely on
    // an implicit library default that a future version could change.
    // WAL improves concurrent read/write throughput and busy_timeout lets
    // contending writers wait briefly instead of surfacing SQLITE_BUSY as a
    // command error (critical for meeting-segment polls racing UI reads).
    let opts = SqliteConnectOptions::from_str(url)?
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    Ok(SqlitePoolOptions::new()
        .max_connections(4)
        .connect_with(opts)
        .await?)
}

static CONFIG_MIGRATOR: sqlx::migrate::Migrator =
    sqlx::migrate!("./migrations/config");

pub async fn run_config_migrations(pool: &SqlitePool) -> Result<(), KeaError> {
    CONFIG_MIGRATOR.run(pool).await?;
    Ok(())
}

static DATA_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/data");

pub async fn run_data_migrations(pool: &SqlitePool) -> Result<(), KeaError> {
    DATA_MIGRATOR.run(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_create_settings_table() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        // settings table exists -> this query succeeds (0 rows)
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }

    // Locks in the data-integrity invariant that a mining round wrongly
    // flagged as broken: deleting a parent must cascade to children. Uses a
    // real file-backed pool (?mode=rwc), the production path, not :memory:.
    #[tokio::test]
    async fn foreign_key_cascade_fires_on_file_backed_pool() {
        let dir = std::env::temp_dir().join("kea_fk_cascade_test");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join(format!("cascade-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&db);
        let url = format!("sqlite://{}?mode=rwc", db.display());

        let pool = open_pool(&url).await.unwrap();
        run_data_migrations(&pool).await.unwrap();

        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(fk, 1, "foreign_keys must be enabled");

        sqlx::query("INSERT INTO meetings (id,title,capture_mode,status,started_at) VALUES ('m1','t','mic_only','recording',datetime('now'))")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO meeting_segments (meeting_id,sequence,start_offset_ms,end_offset_ms,text) VALUES ('m1',0,0,1000,'hi')")
            .execute(&pool).await.unwrap();

        sqlx::query("DELETE FROM meetings WHERE id='m1'")
            .execute(&pool).await.unwrap();
        let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meeting_segments WHERE meeting_id='m1'")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(orphans, 0, "ON DELETE CASCADE did not fire");

        let _ = std::fs::remove_file(&db);
    }
}
