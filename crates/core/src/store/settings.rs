use crate::error::KeaError;
use serde::{de::DeserializeOwned, Serialize};
use sqlx::SqlitePool;

pub struct SettingsRepo {
    pool: SqlitePool,
}

impl SettingsRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, KeaError> {
        let json: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        match json {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    /// Reads a setting whose stored value may itself be the JSON literal
    /// `null` — which is what `set` writes for any `None`. Plain `get::<T>`
    /// fails to deserialize that forever after, so every Option-valued
    /// setting reads through here instead of re-deriving the flatten.
    pub async fn get_optional<T: DeserializeOwned>(
        &self,
        key: &str,
    ) -> Result<Option<T>, KeaError> {
        Ok(self.get::<Option<T>>(key).await?.flatten())
    }

    pub async fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), KeaError> {
        let json = serde_json::to_string(value)?;
        sqlx::query(
            "INSERT INTO settings(key, value) VALUES(?, ?)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(key)
        .bind(json)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn set_then_get_roundtrips() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = SettingsRepo::new(pool);

        repo.set("log_level", &"debug".to_string()).await.unwrap();
        let got: Option<String> = repo.get("log_level").await.unwrap();
        assert_eq!(got, Some("debug".to_string()));

        let missing: Option<String> = repo.get("nope").await.unwrap();
        assert_eq!(missing, None);
    }

    #[tokio::test]
    async fn get_optional_reads_back_a_stored_none() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = SettingsRepo::new(pool);

        // `set` writes None as the JSON literal `null`; plain `get::<String>`
        // would error on that, get_optional flattens it back to None.
        repo.set("active_model", &None::<String>).await.unwrap();
        let cleared: Option<String> = repo.get_optional("active_model").await.unwrap();
        assert_eq!(cleared, None);

        repo.set("active_model", &Some("whisper-base".to_string()))
            .await
            .unwrap();
        let set: Option<String> = repo.get_optional("active_model").await.unwrap();
        assert_eq!(set, Some("whisper-base".to_string()));

        let missing: Option<String> = repo.get_optional("nope").await.unwrap();
        assert_eq!(missing, None);
    }
}
