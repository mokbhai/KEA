use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::KeaError;

/// Where a meeting stands, as persisted in `meetings.status`.
///
/// The spellings below are the column's contract *and* the JSON the UI reads,
/// so they are written once here and never again: `as_str` at the SQL edge,
/// `serde(rename_all)` on the wire. Same shape as `RewriteMode`, for the same
/// reason — a typo ("complete" for "completed") should not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Recording,
    Completed,
    Error,
}

impl MeetingStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MeetingStatus::Recording => "recording",
            MeetingStatus::Completed => "completed",
            MeetingStatus::Error => "error",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "recording" => Some(MeetingStatus::Recording),
            "completed" => Some(MeetingStatus::Completed),
            "error" => Some(MeetingStatus::Error),
            _ => None,
        }
    }
}

impl TryFrom<String> for MeetingStatus {
    type Error = KeaError;

    fn try_from(s: String) -> Result<Self, KeaError> {
        MeetingStatus::from_str(&s)
            .ok_or_else(|| KeaError::Other(format!("unknown meeting status {s:?}")))
    }
}

/// Which sources a meeting recorded from, as persisted in
/// `meetings.capture_mode`. See [`MeetingStatus`] for why it is an enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureMode {
    MicOnly,
    MicAndSystem,
}

impl CaptureMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            CaptureMode::MicOnly => "mic_only",
            CaptureMode::MicAndSystem => "mic_and_system",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "mic_only" => Some(CaptureMode::MicOnly),
            "mic_and_system" => Some(CaptureMode::MicAndSystem),
            _ => None,
        }
    }
}

impl TryFrom<String> for CaptureMode {
    type Error = KeaError;

    fn try_from(s: String) -> Result<Self, KeaError> {
        CaptureMode::from_str(&s)
            .ok_or_else(|| KeaError::Other(format!("unknown capture mode {s:?}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Meeting {
    pub id: String,
    pub title: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    #[sqlx(try_from = "String")]
    pub status: MeetingStatus,
    #[sqlx(try_from = "String")]
    pub capture_mode: CaptureMode,
    pub stt_engine_id: Option<String>,
    pub llm_engine_id: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct MeetingSegment {
    pub id: i64,
    pub meeting_id: String,
    pub sequence: i32,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
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
    pub capture_mode: CaptureMode,
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
            ) VALUES (?, ?, datetime('now'), ?, ?, ?, ?)",
        )
        .bind(&m.id)
        .bind(&m.title)
        .bind(MeetingStatus::Recording.as_str())
        .bind(m.capture_mode.as_str())
        .bind(&m.stt_engine_id)
        .bind(&m.llm_engine_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn list(&self, limit: i64) -> Result<Vec<Meeting>, KeaError> {
        let rows = sqlx::query_as::<_, Meeting>(
            "SELECT id, title, started_at, ended_at, status, capture_mode,
                    stt_engine_id, llm_engine_id, error
             FROM meetings ORDER BY started_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn get(&self, id: &str) -> Result<Option<MeetingDetail>, KeaError> {
        let meeting = sqlx::query_as::<_, Meeting>(
            "SELECT id, title, started_at, ended_at, status, capture_mode,
                    stt_engine_id, llm_engine_id, error
             FROM meetings WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(meeting) = meeting else {
            return Ok(None);
        };

        let segments = sqlx::query_as::<_, MeetingSegment>(
            "SELECT id, meeting_id, sequence, start_offset_ms, end_offset_ms, text
             FROM meeting_segments WHERE meeting_id = ? ORDER BY sequence ASC",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let notes = sqlx::query_as::<_, MeetingNotes>(
            "SELECT meeting_id, summary, decisions, action_items, follow_ups, open_questions,
                    prompt_version, engine_id, model
             FROM meeting_notes WHERE meeting_id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(Some(MeetingDetail {
            meeting,
            segments,
            notes,
        }))
    }

    pub async fn append_segment(
        &self,
        meeting_id: &str,
        seg: &NewSegment,
    ) -> Result<i64, KeaError> {
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
        status: MeetingStatus,
        error: Option<&str>,
    ) -> Result<(), KeaError> {
        let result = sqlx::query(
            "UPDATE meetings SET status = ?, error = ?, ended_at = datetime('now') WHERE id = ?",
        )
        .bind(status.as_str())
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
            capture_mode: CaptureMode::MicOnly,
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
        repo.complete("m1", MeetingStatus::Completed, None)
            .await
            .unwrap();

        let detail = repo.get("m1").await.unwrap().unwrap();
        assert_eq!(detail.meeting.title, "Weekly Sync");
        assert_eq!(detail.segments.len(), 1);
        assert_eq!(detail.segments[0].text, "Hello everyone");
        assert_eq!(detail.notes.as_ref().unwrap().summary, "Kickoff");
    }

    #[test]
    fn status_and_capture_mode_keep_their_stored_spellings() {
        // These strings sit in users' `meetings` rows and in the JSON the UI
        // reads. The enums are a compile-time guard on top of them, never a
        // re-spelling of them.
        for (status, text) in [
            (MeetingStatus::Recording, "recording"),
            (MeetingStatus::Completed, "completed"),
            (MeetingStatus::Error, "error"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(MeetingStatus::from_str(text), Some(status));
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{text}\"")
            );
        }
        for (mode, text) in [
            (CaptureMode::MicOnly, "mic_only"),
            (CaptureMode::MicAndSystem, "mic_and_system"),
        ] {
            assert_eq!(mode.as_str(), text);
            assert_eq!(CaptureMode::from_str(text), Some(mode));
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{text}\""));
        }
    }

    #[tokio::test]
    async fn rows_written_before_the_enums_still_decode() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO meetings(id, title, started_at, status, capture_mode)
             VALUES ('m0', 'Legacy', datetime('now'), 'completed', 'mic_and_system')",
        )
        .execute(&pool)
        .await
        .unwrap();

        let meeting = MeetingRepo::new(pool).list(10).await.unwrap().remove(0);
        assert_eq!(meeting.status, MeetingStatus::Completed);
        assert_eq!(meeting.capture_mode, CaptureMode::MicAndSystem);
    }

    #[tokio::test]
    async fn list_and_delete_meeting() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = MeetingRepo::new(pool);

        repo.create(&NewMeeting {
            id: "m2".into(),
            title: "Standup".into(),
            capture_mode: CaptureMode::MicOnly,
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
