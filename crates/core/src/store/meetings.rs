use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::KeaError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub capture_mode: String,
    pub stt_engine_id: Option<String>,
    pub llm_engine_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingSegment {
    pub id: i64,
    pub meeting_id: String,
    pub sequence: i32,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingNotes {
    pub meeting_id: String,
    pub summary: String,
    pub decisions: String,
    pub action_items: String,
    pub follow_ups: String,
    pub open_questions: String,
    pub prompt_version: String,
    pub engine_id: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingDetail {
    pub meeting: Meeting,
    pub segments: Vec<MeetingSegment>,
    pub notes: Option<MeetingNotes>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewMeeting {
    pub id: String,
    pub title: String,
    pub capture_mode: String,
    pub stt_engine_id: Option<String>,
    pub llm_engine_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewSegment {
    pub sequence: i32,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
    pub text: String,
}

pub struct MeetingRepo {
    pool: SqlitePool,
}

impl MeetingRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, m: &NewMeeting) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO meetings(
                id, title, started_at, status, capture_mode, stt_engine_id, llm_engine_id
            ) VALUES (?, ?, datetime('now'), 'recording', ?, ?, ?)",
        )
        .bind(&m.id)
        .bind(&m.title)
        .bind(&m.capture_mode)
        .bind(&m.stt_engine_id)
        .bind(&m.llm_engine_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self, limit: i64) -> Result<Vec<Meeting>, KeaError> {
        let rows = sqlx::query_as::<
            _,
            (
                String,
                String,
                String,
                Option<String>,
                String,
                String,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >(
            "SELECT id, title, started_at, ended_at, status, capture_mode,
                    stt_engine_id, llm_engine_id, error
             FROM meetings ORDER BY started_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    title,
                    started_at,
                    ended_at,
                    status,
                    capture_mode,
                    stt_engine_id,
                    llm_engine_id,
                    error,
                )| Meeting {
                    id,
                    title,
                    started_at,
                    ended_at,
                    status,
                    capture_mode,
                    stt_engine_id,
                    llm_engine_id,
                    error,
                },
            )
            .collect())
    }

    pub async fn get(&self, id: &str) -> Result<Option<MeetingDetail>, KeaError> {
        let meeting_row: Option<(
            String,
            String,
            String,
            Option<String>,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT id, title, started_at, ended_at, status, capture_mode,
                    stt_engine_id, llm_engine_id, error
             FROM meetings WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let Some((
            id,
            title,
            started_at,
            ended_at,
            status,
            capture_mode,
            stt_engine_id,
            llm_engine_id,
            error,
        )) = meeting_row
        else {
            return Ok(None);
        };

        let meeting = Meeting {
            id: id.clone(),
            title,
            started_at,
            ended_at,
            status,
            capture_mode,
            stt_engine_id,
            llm_engine_id,
            error,
        };

        let segment_rows = sqlx::query_as::<_, (i64, String, i32, i64, i64, String)>(
            "SELECT id, meeting_id, sequence, start_offset_ms, end_offset_ms, text
             FROM meeting_segments WHERE meeting_id = ? ORDER BY sequence ASC",
        )
        .bind(&id)
        .fetch_all(&self.pool)
        .await?;

        let segments = segment_rows
            .into_iter()
            .map(
                |(id, meeting_id, sequence, start_offset_ms, end_offset_ms, text)| MeetingSegment {
                    id,
                    meeting_id,
                    sequence,
                    start_offset_ms,
                    end_offset_ms,
                    text,
                },
            )
            .collect();

        let notes_row: Option<(
            String,
            String,
            String,
            String,
            String,
            String,
            String,
            Option<String>,
            Option<String>,
        )> = sqlx::query_as(
            "SELECT meeting_id, summary, decisions, action_items, follow_ups, open_questions,
                    prompt_version, engine_id, model
             FROM meeting_notes WHERE meeting_id = ?",
        )
        .bind(&id)
        .fetch_optional(&self.pool)
        .await?;

        let notes = notes_row.map(
            |(
                meeting_id,
                summary,
                decisions,
                action_items,
                follow_ups,
                open_questions,
                prompt_version,
                engine_id,
                model,
            )| MeetingNotes {
                meeting_id,
                summary,
                decisions,
                action_items,
                follow_ups,
                open_questions,
                prompt_version,
                engine_id,
                model,
            },
        );

        Ok(Some(MeetingDetail {
            meeting,
            segments,
            notes,
        }))
    }

    pub async fn append_segment(&self, meeting_id: &str, seg: &NewSegment) -> Result<i64, KeaError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO meeting_segments(
                meeting_id, sequence, start_offset_ms, end_offset_ms, text
            ) VALUES (?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(meeting_id)
        .bind(seg.sequence)
        .bind(seg.start_offset_ms)
        .bind(seg.end_offset_ms)
        .bind(&seg.text)
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn upsert_notes(&self, notes: &MeetingNotes) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO meeting_notes(
                meeting_id, summary, decisions, action_items, follow_ups, open_questions,
                prompt_version, engine_id, model
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(meeting_id) DO UPDATE SET
                summary = excluded.summary,
                decisions = excluded.decisions,
                action_items = excluded.action_items,
                follow_ups = excluded.follow_ups,
                open_questions = excluded.open_questions,
                prompt_version = excluded.prompt_version,
                engine_id = excluded.engine_id,
                model = excluded.model",
        )
        .bind(&notes.meeting_id)
        .bind(&notes.summary)
        .bind(&notes.decisions)
        .bind(&notes.action_items)
        .bind(&notes.follow_ups)
        .bind(&notes.open_questions)
        .bind(&notes.prompt_version)
        .bind(&notes.engine_id)
        .bind(&notes.model)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_title(&self, id: &str, title: &str) -> Result<(), KeaError> {
        let result = sqlx::query("UPDATE meetings SET title = ? WHERE id = ?")
            .bind(title)
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::NotFound(format!("meeting {id}")));
        }
        Ok(())
    }

    pub async fn complete(
        &self,
        id: &str,
        status: &str,
        error: Option<&str>,
    ) -> Result<(), KeaError> {
        let result = sqlx::query(
            "UPDATE meetings SET status = ?, error = ?, ended_at = datetime('now') WHERE id = ?",
        )
        .bind(status)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::NotFound(format!("meeting {id}")));
        }
        Ok(())
    }

    pub async fn delete(&self, id: &str) -> Result<(), KeaError> {
        let result = sqlx::query("DELETE FROM meetings WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::NotFound(format!("meeting {id}")));
        }
        Ok(())
    }

    pub async fn prune_older_than_days(&self, days: i64) -> Result<u64, KeaError> {
        let result = sqlx::query(
            "DELETE FROM meetings WHERE started_at < datetime('now', printf('-%d days', ?))",
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
    async fn meetings_tables_exist_after_migration() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meetings")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn meeting_roundtrip_with_segments_and_notes() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = MeetingRepo::new(pool);

        repo.create(&NewMeeting {
            id: "m1".into(),
            title: "Untitled Meeting".into(),
            capture_mode: "mic_only".into(),
            stt_engine_id: Some("openai-stt".into()),
            llm_engine_id: Some("openai".into()),
        })
        .await
        .unwrap();

        let seg_id = repo
            .append_segment(
                "m1",
                &NewSegment {
                    sequence: 0,
                    start_offset_ms: 0,
                    end_offset_ms: 30_000,
                    text: "Hello everyone".into(),
                },
            )
            .await
            .unwrap();
        assert!(seg_id > 0);

        repo.upsert_notes(&MeetingNotes {
            meeting_id: "m1".into(),
            summary: "Kickoff".into(),
            decisions: "".into(),
            action_items: "Follow up".into(),
            follow_ups: "".into(),
            open_questions: "".into(),
            prompt_version: "meeting-notes-v1".into(),
            engine_id: Some("openai".into()),
            model: Some("gpt-4o-mini".into()),
        })
        .await
        .unwrap();

        repo.set_title("m1", "Weekly Sync").await.unwrap();
        repo.complete("m1", "completed", None).await.unwrap();

        let detail = repo.get("m1").await.unwrap().unwrap();
        assert_eq!(detail.meeting.title, "Weekly Sync");
        assert_eq!(detail.segments.len(), 1);
        assert_eq!(detail.segments[0].text, "Hello everyone");
        assert_eq!(detail.notes.as_ref().unwrap().summary, "Kickoff");
    }

    #[tokio::test]
    async fn list_and_delete_meeting() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = MeetingRepo::new(pool);

        repo.create(&NewMeeting {
            id: "m2".into(),
            title: "Standup".into(),
            capture_mode: "mic_only".into(),
            stt_engine_id: None,
            llm_engine_id: None,
        })
        .await
        .unwrap();

        let meetings = repo.list(10).await.unwrap();
        assert_eq!(meetings.len(), 1);
        assert_eq!(meetings[0].id, "m2");

        repo.delete("m2").await.unwrap();
        assert!(repo.get("m2").await.unwrap().is_none());
    }
}
