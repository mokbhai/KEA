use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::KeaError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationSummary {
    pub id: i64,
    pub action_id: Option<i64>,
    pub feature_id: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: i64,
    pub conversation_id: i64,
    pub role: String,
    pub content: String,
    pub token_count: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewConversation {
    pub action_id: Option<i64>,
    pub feature_id: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewMessage {
    pub conversation_id: i64,
    pub role: String,
    pub content: String,
    pub token_count: Option<i64>,
}

pub struct ConversationRepo {
    pool: SqlitePool,
}

impl ConversationRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn start(&self, c: &NewConversation) -> Result<i64, KeaError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO conversations(action_id, feature_id, engine_id, model, provider_ref)
             VALUES(?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(c.action_id)
        .bind(&c.feature_id)
        .bind(&c.engine_id)
        .bind(&c.model)
        .bind(&c.provider_ref)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn append_message(&self, msg: &NewMessage) -> Result<i64, KeaError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO messages(conversation_id, role, content, token_count)
             VALUES(?, ?, ?, ?) RETURNING id",
        )
        .bind(msg.conversation_id)
        .bind(&msg.role)
        .bind(&msg.content)
        .bind(msg.token_count)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn list_recent(&self, limit: i64) -> Result<Vec<ConversationSummary>, KeaError> {
        let rows = sqlx::query_as::<
            _,
            (
                i64,
                Option<i64>,
                String,
                String,
                Option<String>,
                Option<String>,
                String,
            ),
        >(
            "SELECT id, action_id, feature_id, engine_id, model, provider_ref, created_at
             FROM conversations ORDER BY created_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, action_id, feature_id, engine_id, model, provider_ref, created_at)| {
                    ConversationSummary {
                        id,
                        action_id,
                        feature_id,
                        engine_id,
                        model,
                        provider_ref,
                        created_at,
                    }
                },
            )
            .collect())
    }

    pub async fn list_messages(&self, conversation_id: i64) -> Result<Vec<Message>, KeaError> {
        let rows = sqlx::query_as::<_, (i64, i64, String, String, Option<i64>, String)>(
            "SELECT id, conversation_id, role, content, token_count, created_at
             FROM messages WHERE conversation_id = ? ORDER BY id",
        )
        .bind(conversation_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(id, conversation_id, role, content, token_count, created_at)| Message {
                    id,
                    conversation_id,
                    role,
                    content,
                    token_count,
                    created_at,
                },
            )
            .collect())
    }

    pub async fn delete_conversation(&self, id: i64) -> Result<(), KeaError> {
        let result = sqlx::query("DELETE FROM conversations WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::NotFound(format!("conversation {id}")));
        }
        Ok(())
    }

    pub async fn prune_older_than_days(&self, days: i64) -> Result<u64, KeaError> {
        let result = sqlx::query(
            "DELETE FROM conversations WHERE created_at < datetime('now', printf('-%d days', ?))",
        )
        .bind(days)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_data_migrations};

    #[tokio::test]
    async fn conversations_tables_exist_after_migration() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let conv_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversations")
            .fetch_one(&pool)
            .await
            .unwrap();
        let msg_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(conv_count, 0);
        assert_eq!(msg_count, 0);
    }

    #[tokio::test]
    async fn append_message_and_list_recent() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ConversationRepo::new(pool);

        let conv_id = repo
            .start(&NewConversation {
                action_id: None,
                feature_id: "rewrite".into(),
                engine_id: "openai".into(),
                model: Some("gpt-4o-mini".into()),
                provider_ref: Some("openai".into()),
            })
            .await
            .unwrap();

        repo.append_message(&NewMessage {
            conversation_id: conv_id,
            role: "user".into(),
            content: "hello".into(),
            token_count: Some(1),
        })
        .await
        .unwrap();
        repo.append_message(&NewMessage {
            conversation_id: conv_id,
            role: "assistant".into(),
            content: "hi there".into(),
            token_count: Some(2),
        })
        .await
        .unwrap();

        let recent = repo.list_recent(10).await.unwrap();
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].feature_id, "rewrite");
        assert_eq!(recent[0].engine_id, "openai");

        let messages = repo.list_messages(conv_id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[1].role, "assistant");
    }

    #[tokio::test]
    async fn delete_conversation_removes_row_and_messages() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ConversationRepo::new(pool.clone());

        let conv_id = repo
            .start(&NewConversation {
                action_id: None,
                feature_id: "rewrite".into(),
                engine_id: "openai".into(),
                model: Some("gpt-4o-mini".into()),
                provider_ref: Some("openai".into()),
            })
            .await
            .unwrap();

        repo.append_message(&NewMessage {
            conversation_id: conv_id,
            role: "user".into(),
            content: "test".into(),
            token_count: None,
        })
        .await
        .unwrap();

        repo.delete_conversation(conv_id).await.unwrap();

        let recent = repo.list_recent(10).await.unwrap();
        assert!(recent.is_empty());

        let messages = repo.list_messages(conv_id).await.unwrap();
        assert!(messages.is_empty());
    }

    #[tokio::test]
    async fn delete_conversation_not_found() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ConversationRepo::new(pool);

        let err = repo.delete_conversation(999).await.unwrap_err();
        assert!(matches!(err, KeaError::NotFound(_)));
    }
}
