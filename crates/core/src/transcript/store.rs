//! Persistence for file transcripts.

use kea_engines::traits::SttSegment;
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::error::KeaError;

/// Where a file transcription stands, as persisted in `transcripts.status`.
///
/// An enum with `as_str` at the SQL edge and `serde(rename_all)` on the wire,
/// the shape `MeetingStatus` and `CaptureMode` use: the stored spelling is
/// written once and a typo cannot compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    Running,
    Completed,
    /// The user stopped it. Distinct from `Error` because the partial
    /// transcript is intact and worth exporting, which a failed one is not.
    Cancelled,
    Error,
}

impl TranscriptStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TranscriptStatus::Running => "running",
            TranscriptStatus::Completed => "completed",
            TranscriptStatus::Cancelled => "cancelled",
            TranscriptStatus::Error => "error",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(TranscriptStatus::Running),
            "completed" => Some(TranscriptStatus::Completed),
            "cancelled" => Some(TranscriptStatus::Cancelled),
            "error" => Some(TranscriptStatus::Error),
            _ => None,
        }
    }
}

impl TryFrom<String> for TranscriptStatus {
    type Error = KeaError;

    fn try_from(s: String) -> Result<Self, KeaError> {
        TranscriptStatus::from_str(&s)
            .ok_or_else(|| KeaError::Other(format!("unknown transcript status {s:?}")))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct TranscriptRow {
    pub id: String,
    pub source_path: String,
    pub source_filename: String,
    pub duration_ms: i64,
    pub stt_engine_id: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
    #[sqlx(try_from = "String")]
    pub status: TranscriptStatus,
    pub error: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct TranscriptSegmentRow {
    pub id: i64,
    pub transcript_id: String,
    pub sequence: i32,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub speaker_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptDetail {
    pub transcript: TranscriptRow,
    pub segments: Vec<TranscriptSegmentRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTranscript {
    pub id: String,
    pub source_path: String,
    pub source_filename: String,
    pub stt_engine_id: Option<String>,
    pub model: Option<String>,
    pub language: Option<String>,
}

pub struct TranscriptRepo {
    pool: SqlitePool,
}

impl TranscriptRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, t: &NewTranscript) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO transcripts(
                id, source_path, source_filename, stt_engine_id, model, language, status
            ) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&t.id)
        .bind(&t.source_path)
        .bind(&t.source_filename)
        .bind(&t.stt_engine_id)
        .bind(&t.model)
        .bind(&t.language)
        .bind(TranscriptStatus::Running.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Replaces the cue list in one transaction.
    ///
    /// Replace rather than append: a re-run of the same job must not leave
    /// the old cues interleaved with the new ones, and `UNIQUE(transcript_id,
    /// sequence)` would reject the second write anyway.
    pub async fn replace_segments(
        &self,
        transcript_id: &str,
        segments: &[SttSegment],
        speakers: &[Option<String>],
    ) -> Result<(), KeaError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM transcript_segments WHERE transcript_id = ?")
            .bind(transcript_id)
            .execute(&mut *tx)
            .await?;
        for (index, segment) in segments.iter().enumerate() {
            sqlx::query(
                "INSERT INTO transcript_segments(
                    transcript_id, sequence, start_ms, end_ms, text, speaker_key
                ) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(transcript_id)
            .bind(index as i32)
            .bind(segment.start_ms as i64)
            .bind(segment.end_ms as i64)
            .bind(&segment.text)
            .bind(speakers.get(index).cloned().flatten())
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn complete(
        &self,
        id: &str,
        status: TranscriptStatus,
        duration_ms: i64,
        error: Option<&str>,
    ) -> Result<(), KeaError> {
        let result = sqlx::query(
            "UPDATE transcripts SET status = ?, duration_ms = ?, error = ? WHERE id = ?",
        )
        .bind(status.as_str())
        .bind(duration_ms)
        .bind(error)
        .bind(id)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::NotFound(format!("transcript {id}")));
        }
        Ok(())
    }

    pub async fn list(&self, limit: i64) -> Result<Vec<TranscriptRow>, KeaError> {
        Ok(sqlx::query_as::<_, TranscriptRow>(
            "SELECT id, source_path, source_filename, duration_ms, stt_engine_id, model,
                    language, status, error, created_at
             FROM transcripts ORDER BY created_at DESC, rowid DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn get(&self, id: &str) -> Result<Option<TranscriptDetail>, KeaError> {
        let transcript = sqlx::query_as::<_, TranscriptRow>(
            "SELECT id, source_path, source_filename, duration_ms, stt_engine_id, model,
                    language, status, error, created_at
             FROM transcripts WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(transcript) = transcript else {
            return Ok(None);
        };

        let segments = sqlx::query_as::<_, TranscriptSegmentRow>(
            "SELECT id, transcript_id, sequence, start_ms, end_ms, text, speaker_key
             FROM transcript_segments WHERE transcript_id = ? ORDER BY sequence ASC",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        Ok(Some(TranscriptDetail {
            transcript,
            segments,
        }))
    }

    pub async fn delete(&self, id: &str) -> Result<(), KeaError> {
        let result = sqlx::query("DELETE FROM transcripts WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::NotFound(format!("transcript {id}")));
        }
        Ok(())
    }
}

/// The cue list a row set represents, back in engine shape, so the subtitle
/// writers take the same type whether the cues came from a live run or from
/// the database.
pub fn segments_from_rows(rows: &[TranscriptSegmentRow]) -> Vec<SttSegment> {
    rows.iter()
        .map(|r| SttSegment {
            start_ms: r.start_ms.max(0) as u64,
            end_ms: r.end_ms.max(0) as u64,
            text: r.text.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_data_migrations};

    async fn repo() -> TranscriptRepo {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        TranscriptRepo::new(pool)
    }

    fn new_transcript(id: &str) -> NewTranscript {
        NewTranscript {
            id: id.into(),
            source_path: format!("/tmp/{id}.m4a"),
            source_filename: format!("{id}.m4a"),
            stt_engine_id: Some("whisper".into()),
            model: Some("ggml-base.en".into()),
            language: Some("en".into()),
        }
    }

    #[tokio::test]
    async fn a_transcript_roundtrips_with_its_cues() {
        let repo = repo().await;
        repo.create(&new_transcript("t1")).await.unwrap();
        repo.replace_segments(
            "t1",
            &[
                SttSegment::new(0, 1_500, "hello"),
                SttSegment::new(1_500, 3_000, "world"),
            ],
            &[Some("local".into()), None],
        )
        .await
        .unwrap();
        repo.complete("t1", TranscriptStatus::Completed, 3_000, None)
            .await
            .unwrap();

        let detail = repo.get("t1").await.unwrap().unwrap();
        assert_eq!(detail.transcript.status, TranscriptStatus::Completed);
        assert_eq!(detail.transcript.duration_ms, 3_000);
        assert_eq!(detail.segments.len(), 2);
        assert_eq!(detail.segments[0].speaker_key.as_deref(), Some("local"));
        assert_eq!(detail.segments[1].speaker_key, None);
        assert_eq!(segments_from_rows(&detail.segments)[1].text, "world");
    }

    /// A re-run must replace the cue list, not interleave with the old one.
    #[tokio::test]
    async fn replacing_segments_clears_the_previous_run() {
        let repo = repo().await;
        repo.create(&new_transcript("t2")).await.unwrap();
        repo.replace_segments("t2", &[SttSegment::new(0, 1_000, "old")], &[])
            .await
            .unwrap();
        repo.replace_segments(
            "t2",
            &[
                SttSegment::new(0, 1_000, "new"),
                SttSegment::new(1_000, 2_000, "newer"),
            ],
            &[],
        )
        .await
        .unwrap();
        let detail = repo.get("t2").await.unwrap().unwrap();
        assert_eq!(detail.segments.len(), 2);
        assert_eq!(detail.segments[0].text, "new");
    }

    /// A cancel leaves a `cancelled` row and the cues it did manage.
    #[tokio::test]
    async fn a_cancelled_run_keeps_its_partial_cues() {
        let repo = repo().await;
        repo.create(&new_transcript("t3")).await.unwrap();
        repo.replace_segments("t3", &[SttSegment::new(0, 30_000, "first chunk")], &[])
            .await
            .unwrap();
        repo.complete("t3", TranscriptStatus::Cancelled, 30_000, None)
            .await
            .unwrap();
        let detail = repo.get("t3").await.unwrap().unwrap();
        assert_eq!(detail.transcript.status, TranscriptStatus::Cancelled);
        assert_eq!(detail.segments.len(), 1);
    }

    #[tokio::test]
    async fn deleting_a_transcript_cascades_to_its_cues() {
        let repo = repo().await;
        repo.create(&new_transcript("t4")).await.unwrap();
        repo.replace_segments("t4", &[SttSegment::new(0, 1_000, "x")], &[])
            .await
            .unwrap();
        repo.delete("t4").await.unwrap();
        assert!(repo.get("t4").await.unwrap().is_none());
        let orphans: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM transcript_segments WHERE transcript_id = ?")
                .bind("t4")
                .fetch_one(&repo.pool)
                .await
                .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn status_keeps_its_stored_spelling() {
        for (status, text) in [
            (TranscriptStatus::Running, "running"),
            (TranscriptStatus::Completed, "completed"),
            (TranscriptStatus::Cancelled, "cancelled"),
            (TranscriptStatus::Error, "error"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(TranscriptStatus::from_str(text), Some(status));
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{text}\"")
            );
        }
    }
}
