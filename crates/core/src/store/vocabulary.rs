use sqlx::SqlitePool;

use crate::error::KeaError;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct VocabularyEntry {
    pub id: String,
    pub term: String,
    pub sounds_like: Option<String>,
    pub enabled: bool,
    pub created_at: String,
}

pub struct VocabularyRepo {
    pool: SqlitePool,
}

impl VocabularyRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn upsert(&self, entry: &VocabularyEntry) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO vocabulary(id, term, sounds_like, enabled, created_at)
             VALUES(?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET
                 term = excluded.term,
                 sounds_like = excluded.sounds_like,
                 enabled = excluded.enabled",
        )
        .bind(&entry.id)
        .bind(&entry.term)
        .bind(&entry.sounds_like)
        .bind(entry.enabled)
        .bind(&entry.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Ordered by term, case-insensitively, so the settings table keeps a
    /// stable row order across saves instead of reshuffling on every write.
    pub async fn list(&self) -> Result<Vec<VocabularyEntry>, KeaError> {
        let rows = sqlx::query_as::<_, VocabularyEntry>(
            "SELECT id, term, sounds_like, enabled, created_at
             FROM vocabulary ORDER BY term COLLATE NOCASE",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// The transcription path only ever wants the active entries, and letting
    /// every caller re-filter is how one of them eventually forgets to.
    pub async fn list_enabled(&self) -> Result<Vec<VocabularyEntry>, KeaError> {
        let rows = sqlx::query_as::<_, VocabularyEntry>(
            "SELECT id, term, sounds_like, enabled, created_at
             FROM vocabulary WHERE enabled = 1 ORDER BY term COLLATE NOCASE",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn get(&self, id: &str) -> Result<Option<VocabularyEntry>, KeaError> {
        let row = sqlx::query_as::<_, VocabularyEntry>(
            "SELECT id, term, sounds_like, enabled, created_at FROM vocabulary WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn delete(&self, id: &str) -> Result<(), KeaError> {
        sqlx::query("DELETE FROM vocabulary WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    fn entry(id: &str, term: &str, sounds_like: Option<&str>, enabled: bool) -> VocabularyEntry {
        VocabularyEntry {
            id: id.into(),
            term: term.into(),
            sounds_like: sounds_like.map(str::to_string),
            enabled,
            created_at: "2026-09-19T10:00:00Z".into(),
        }
    }

    async fn repo() -> VocabularyRepo {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        VocabularyRepo::new(pool)
    }

    #[tokio::test]
    async fn vocabulary_roundtrip() {
        let repo = repo().await;
        let e = entry("v1", "KittyClaw", Some("kitty claw, kitty-claw"), true);
        repo.upsert(&e).await.unwrap();

        assert_eq!(repo.get("v1").await.unwrap(), Some(e.clone()));
        assert_eq!(repo.list().await.unwrap(), vec![e]);

        repo.delete("v1").await.unwrap();
        assert_eq!(repo.get("v1").await.unwrap(), None);
        assert!(repo.list().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn upsert_replaces_an_existing_id() {
        let repo = repo().await;
        repo.upsert(&entry("v1", "KEA", None, true)).await.unwrap();
        repo.upsert(&entry("v1", "KEA", Some("kia, kaya"), false))
            .await
            .unwrap();

        let all = repo.list().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].sounds_like.as_deref(), Some("kia, kaya"));
        assert!(!all[0].enabled);
    }

    #[tokio::test]
    async fn list_is_case_insensitively_ordered_by_term() {
        let repo = repo().await;
        repo.upsert(&entry("v1", "zebra", None, true))
            .await
            .unwrap();
        repo.upsert(&entry("v2", "Apple", None, true))
            .await
            .unwrap();
        repo.upsert(&entry("v3", "mango", None, true))
            .await
            .unwrap();

        let terms: Vec<_> = repo
            .list()
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.term)
            .collect();
        assert_eq!(terms, vec!["Apple", "mango", "zebra"]);
    }

    #[tokio::test]
    async fn list_enabled_skips_disabled_rows() {
        let repo = repo().await;
        repo.upsert(&entry("v1", "KittyClaw", None, true))
            .await
            .unwrap();
        repo.upsert(&entry("v2", "Parakeet", None, false))
            .await
            .unwrap();

        assert_eq!(repo.list().await.unwrap().len(), 2);
        let enabled = repo.list_enabled().await.unwrap();
        assert_eq!(enabled.len(), 1);
        assert_eq!(enabled[0].term, "KittyClaw");
    }

    #[tokio::test]
    async fn duplicate_term_is_rejected_case_insensitively() {
        let repo = repo().await;
        repo.upsert(&entry("v1", "KittyClaw", None, true))
            .await
            .unwrap();

        // The unique index is COLLATE NOCASE precisely so a second entry cannot
        // disagree with the first about how the same word should be spelled.
        let clash = repo.upsert(&entry("v2", "kittyclaw", None, true)).await;
        assert!(clash.is_err());
        assert_eq!(repo.list().await.unwrap().len(), 1);
    }
}
