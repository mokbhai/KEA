use serde::{de::DeserializeOwned, Serialize};
use sqlx::SqlitePool;
use crate::error::KeaError;

pub struct SettingsRepo { pool: SqlitePool }

impl SettingsRepo {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, KeaError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM settings WHERE key = ?")
                .bind(key).fetch_optional(&self.pool).await?;
        match row {
            Some((json,)) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    pub async fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), KeaError> {
        let json = serde_json::to_string(value)?;
        sqlx::query("INSERT INTO settings(key, value) VALUES(?, ?)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(key).bind(json).execute(&self.pool).await?;
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
}
