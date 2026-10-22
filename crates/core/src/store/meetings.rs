use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sqlx::{Sqlite, SqlitePool, Transaction};

use crate::error::KeaError;
use crate::meetings::{render_action_items_prose, ParsedActionItem, SpeakerChannel};
use crate::store::conversations::MessageRole;

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

/// Who chose a speaker's display name, as persisted in
/// `meeting_speakers.source`. See [`MeetingStatus`] for why it is an enum.
///
/// The distinction is load-bearing rather than informational: a name a human
/// typed must survive anything attribution decides afterwards.
///
/// Precedence is `User` > `Inferred` > `Channel`. Inference may replace a
/// default but never a typed name, which is why it is its own value rather
/// than a second `Channel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerSource {
    /// The default this feature wrote when the meeting started, or when a
    /// participant's voice first appeared.
    Channel,
    /// A name the user typed.
    User,
    /// A name the LLM proposed from the transcript at stop. Shown with a
    /// confirm action, and replaced by any name the user types.
    Inferred,
}

impl SpeakerSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            SpeakerSource::Channel => "channel",
            SpeakerSource::User => "user",
            SpeakerSource::Inferred => "inferred",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "channel" => Some(SpeakerSource::Channel),
            "user" => Some(SpeakerSource::User),
            "inferred" => Some(SpeakerSource::Inferred),
            _ => None,
        }
    }
}

impl TryFrom<String> for SpeakerSource {
    type Error = KeaError;

    fn try_from(s: String) -> Result<Self, KeaError> {
        SpeakerSource::from_str(&s)
            .ok_or_else(|| KeaError::Other(format!("unknown speaker source {s:?}")))
    }
}

/// Where a meeting's title came from, as persisted in `meetings.title_source`.
/// See [`MeetingStatus`] for why it is an enum.
///
/// `Llm` is also what a NULL column reads as — every row written before
/// calendar titles existed was named by the summariser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TitleSource {
    /// Copied verbatim from a matched calendar event. Never sent to an engine.
    Calendar,
    /// Generated by the bound LLM from the meeting summary.
    Llm,
    /// Typed by a human, and therefore never overwritten by either of the above.
    User,
}

impl TitleSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            TitleSource::Calendar => "calendar",
            TitleSource::Llm => "llm",
            TitleSource::User => "user",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "calendar" => Some(TitleSource::Calendar),
            "llm" => Some(TitleSource::Llm),
            "user" => Some(TitleSource::User),
            _ => None,
        }
    }
}

impl TryFrom<String> for TitleSource {
    type Error = KeaError;

    fn try_from(s: String) -> Result<Self, KeaError> {
        TitleSource::from_str(&s)
            .ok_or_else(|| KeaError::Other(format!("unknown title source {s:?}")))
    }
}

/// Where one action item stands, as persisted in
/// `meeting_action_items.status`. See [`MeetingStatus`] for why it is an enum.
///
/// `Dropped` is distinct from deleting the row on purpose: a re-synthesis pass
/// must be able to tell "the user does not want this" from "the user has not
/// seen this yet", and only the former stops the item coming back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionItemStatus {
    Open,
    Done,
    Dropped,
}

impl ActionItemStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionItemStatus::Open => "open",
            ActionItemStatus::Done => "done",
            ActionItemStatus::Dropped => "dropped",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "open" => Some(ActionItemStatus::Open),
            "done" => Some(ActionItemStatus::Done),
            "dropped" => Some(ActionItemStatus::Dropped),
            _ => None,
        }
    }
}

impl TryFrom<String> for ActionItemStatus {
    type Error = KeaError;

    fn try_from(s: String) -> Result<Self, KeaError> {
        ActionItemStatus::from_str(&s)
            .ok_or_else(|| KeaError::Other(format!("unknown action item status {s:?}")))
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
    /// Who named this meeting. Every `SELECT` below reads it through
    /// `COALESCE(title_source, 'llm')`, so the NULL that every pre-existing
    /// row carries decodes without a hand-written `Option` mapping.
    #[sqlx(try_from = "String")]
    pub title_source: TitleSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct MeetingSegment {
    pub id: i64,
    pub meeting_id: String,
    pub sequence: i32,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
    pub text: String,
    /// Which speaker said this, joined to [`MeetingSpeaker::speaker_key`].
    ///
    /// A plain `String` rather than [`SpeakerChannel`] because the column is
    /// deliberately an open set: channel attribution writes `local`/`remote`
    /// today, and model diarization will write `spk0`, `spk1`, … into the same
    /// column. `None` is "unknown", which is every row written before
    /// attribution existed.
    pub speaker_key: Option<String>,
}

/// One side of a meeting, and what to call it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct MeetingSpeaker {
    pub meeting_id: String,
    pub speaker_key: String,
    pub display_name: String,
    #[sqlx(try_from = "String")]
    pub source: SpeakerSource,
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
    /// Whether a user changed these notes since generation. Set by
    /// [`MeetingRepo::apply_user_notes`]; generation clears it. Read as an
    /// integer column, so a row written before migration 0009 decodes as
    /// `false`.
    pub edited: bool,
}

/// The editable text of a meeting's notes: what a user change replaces, and
/// what a chat proposal carries. The prompt version and engine stay with the
/// generation that wrote them, so they are not part of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotesText {
    pub summary: String,
    pub decisions: String,
    pub action_items: String,
    pub follow_ups: String,
    pub open_questions: String,
}

/// Where a notes proposal stands. A proposal starts `Pending` and leaves it
/// once, for `Applied` or `Discarded`; [`MeetingRepo::set_proposal_state`]
/// refuses anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalState {
    Pending,
    Applied,
    Discarded,
}

impl ProposalState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProposalState::Pending => "pending",
            ProposalState::Applied => "applied",
            ProposalState::Discarded => "discarded",
        }
    }
}

/// The proposal an assistant message carried, and what became of it. This is
/// the JSON stored in `messages.meta`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalMeta {
    pub proposal: NotesText,
    pub state: ProposalState,
    /// The sections the model actually changed: `summary`, `decisions`,
    /// `follow_ups`, `open_questions`. Apply writes only these, onto the notes
    /// as they are *then*, so a hand edit made to another section after the
    /// proposal arrived is not silently reverted by applying it. Empty reads
    /// as "every section", for a proposal stored without the list.
    #[serde(default)]
    pub changed: Vec<String>,
}

/// One message of a meeting's chat, with its proposal when it has one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingChatMessage {
    pub id: i64,
    pub role: MessageRole,
    pub content: String,
    pub proposal: Option<ProposalMeta>,
    pub created_at: String,
}

/// One thing somebody agreed to do, as a row rather than a line of prose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct ActionItem {
    pub id: i64,
    pub meeting_id: String,
    pub text: String,
    /// Who owns it, as the model named them. `None` when nobody was named —
    /// never the meeting host by default, which would be an invention.
    pub owner: Option<String>,
    /// Verbatim from the transcript ("by Friday"), never resolved to a date.
    pub due_hint: Option<String>,
    /// The last `meeting_segments.sequence` the pass that first wrote the item
    /// had read, so a later pass can show roughly where it came from. `None`
    /// for a hand-added item, which is also what keeps a later pass from
    /// rewording or removing it.
    pub source_seq: Option<i64>,
    #[sqlx(try_from = "String")]
    pub status: ActionItemStatus,
}

/// An action item on its way into the table, before it has an id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NewActionItem {
    pub text: String,
    pub owner: Option<String>,
    pub due_hint: Option<String>,
    pub source_seq: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingDetail {
    pub meeting: Meeting,
    pub segments: Vec<MeetingSegment>,
    pub notes: Option<MeetingNotes>,
    /// The meeting's speakers, ordered by key. Empty for meetings recorded
    /// before attribution existed.
    pub speakers: Vec<MeetingSpeaker>,
    /// Action items as rows, oldest first. Empty for meetings recorded before
    /// this table existed — `notes.action_items` still carries their prose.
    pub action_items: Vec<ActionItem>,
    /// Whether [`MeetingRepo::undo_notes`] has anything to restore. Read from
    /// the undo stack, so it is false while the notes are as generated.
    pub can_undo: bool,
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
    /// The `meeting_speakers.speaker_key` this line belongs to. `None` when the
    /// sources could not be told apart — an honest "unknown", never a default
    /// side.
    ///
    /// A plain key rather than a [`SpeakerChannel`] because voice separation
    /// writes `spk0`, `spk1`, … into the same column. Channel segments pass
    /// [`SpeakerChannel::as_str`].
    pub speaker_key: Option<String>,
}

/// The identity of an action item for deduplication purposes.
///
/// Case, surrounding whitespace, runs of internal whitespace, leading bullet
/// markers and trailing punctuation are all things a model varies between
/// passes while meaning the same task, so none of them are part of the key.
pub fn normalize_action_key(text: &str) -> String {
    let trimmed = text.trim().trim_start_matches(['-', '*', '\u{2022}', ' ']);
    let words: Vec<String> = trimmed
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();
    words
        .join(" ")
        .trim_end_matches(['.', '!', ';', ':'])
        .to_string()
}

/// Whether two [`normalize_action_key`] keys name the same task.
///
/// Equal keys do, and so does a key that extends the other at a word
/// boundary: a fold restating "grant confluence access" as "grant confluence
/// access; it will take a while to come through" has added detail, not a task.
pub fn same_action(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if short.is_empty() {
        return false;
    }
    long.strip_prefix(short)
        .is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_alphanumeric()))
}

/// One segment's place after the stop-time re-cluster: the participant key it
/// now carries. Addressed by `sequence`, which is unique within a meeting.
///
/// Per-segment rather than per-key because the pass can split one live key in
/// two as well as merge two into one, and a key-to-key map cannot express the
/// first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentRelabel {
    pub sequence: i32,
    pub speaker_key: String,
}

/// The name a participant currently carries, read inside the caller's
/// transaction so the rename and the owner carry-over see the same state.
async fn speaker_name_in(
    tx: &mut Transaction<'_, Sqlite>,
    meeting_id: &str,
    speaker_key: &str,
) -> Result<Option<String>, KeaError> {
    let name = sqlx::query_scalar(
        "SELECT display_name FROM meeting_speakers WHERE meeting_id = ? AND speaker_key = ?",
    )
    .bind(meeting_id)
    .bind(speaker_key)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(name)
}

/// Point every action-item owner that quoted `old` at `new`, then re-render the
/// prose column from the rows.
///
/// Exact match on purpose: "Dan" must not rewrite an owner that says "Daniel",
/// because nobody renamed that one. The prose is re-rendered with the same
/// function `persist_notes` uses, so the column reads identically whichever
/// path wrote it last. No LLM call is involved; this only rewrites text.
async fn carry_owner_rename(
    tx: &mut Transaction<'_, Sqlite>,
    meeting_id: &str,
    old: &str,
    new: &str,
) -> Result<(), KeaError> {
    let changed =
        sqlx::query("UPDATE meeting_action_items SET owner = ? WHERE meeting_id = ? AND owner = ?")
            .bind(new)
            .bind(meeting_id)
            .bind(old)
            .execute(&mut **tx)
            .await?
            .rows_affected();
    if changed == 0 {
        return Ok(());
    }

    let rows = sqlx::query_as::<_, ActionItem>(
        "SELECT id, meeting_id, text, owner, due_hint, source_seq, status
         FROM meeting_action_items WHERE meeting_id = ? ORDER BY id ASC",
    )
    .bind(meeting_id)
    .fetch_all(&mut **tx)
    .await?;
    let parsed: Vec<ParsedActionItem> = rows
        .into_iter()
        .map(|row| ParsedActionItem {
            text: row.text,
            owner: row.owner,
            due_hint: row.due_hint,
        })
        .collect();

    sqlx::query("UPDATE meeting_notes SET action_items = ? WHERE meeting_id = ?")
        .bind(render_action_items_prose(&parsed))
        .bind(meeting_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The key that most of `from`'s segments now carry, with ties going to the
/// smaller key so the result does not depend on row order.
fn majority_target(
    before: &[(i32, Option<String>)],
    after: &HashMap<i32, Option<String>>,
    from: &str,
) -> Option<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (sequence, key) in before {
        if key.as_deref() != Some(from) {
            continue;
        }
        if let Some(Some(target)) = after.get(sequence) {
            *counts.entry(target.clone()).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(key, _)| key)
}

/// A chat message row before its `meta` is decoded. Kept private so the
/// public [`MeetingChatMessage`] is the only shape callers see.
#[derive(sqlx::FromRow)]
struct ChatRow {
    id: i64,
    #[sqlx(try_from = "String")]
    role: MessageRole,
    content: String,
    meta: Option<String>,
    created_at: String,
}

impl ChatRow {
    fn into_message(self) -> Result<MeetingChatMessage, KeaError> {
        let proposal = self
            .meta
            .as_deref()
            .map(serde_json::from_str::<ProposalMeta>)
            .transpose()?;
        Ok(MeetingChatMessage {
            id: self.id,
            role: self.role,
            content: self.content,
            proposal,
            created_at: self.created_at,
        })
    }
}

/// How many versions the notes undo stack keeps per meeting. Past this the
/// oldest *edit* is dropped, which bounds the table for a user who keeps
/// iterating on one meeting. The bottom of the stack — the notes as
/// generated — is never dropped: Undo must always be able to get back there
/// and clear the edited mark, however many changes came in between.
pub const NOTES_UNDO_LIMIT: i64 = 50;

const NOTES_SELECT: &str = "SELECT meeting_id, summary, decisions, action_items, follow_ups,
            open_questions, prompt_version, engine_id, model, edited
     FROM meeting_notes WHERE meeting_id = ?";

/// `LIKE` treats `%` and `_` as wildcards, so a user's query has to escape
/// them, and the escape character itself, before it is wrapped in `%…%`.
fn escape_like(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// The write behind every user change to the notes, inside the caller's
/// transaction.
///
/// Pushes the current notes onto the undo stack, trims the stack to its cap,
/// then writes the new text with `edited = 1`. Kept separate from the public
/// methods so an applied proposal can do its state change in the same
/// transaction. Fails with `NotFound` when there are no notes to replace.
async fn write_user_notes(
    tx: &mut Transaction<'_, Sqlite>,
    meeting_id: &str,
    text: &NotesText,
) -> Result<(), KeaError> {
    let current = sqlx::query_as::<_, MeetingNotes>(NOTES_SELECT)
        .bind(meeting_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| KeaError::NotFound(format!("notes for meeting {meeting_id}")))?;

    sqlx::query(
        "INSERT INTO meeting_notes_versions (meeting_id, notes_json, edited)
         VALUES (?, ?, ?)",
    )
    .bind(meeting_id)
    .bind(serde_json::to_string(&current)?)
    .bind(i64::from(current.edited))
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "DELETE FROM meeting_notes_versions
         WHERE meeting_id = ?
           AND id <> (SELECT MIN(id) FROM meeting_notes_versions WHERE meeting_id = ?)
           AND id NOT IN (
               SELECT id FROM meeting_notes_versions
               WHERE meeting_id = ? ORDER BY id DESC LIMIT ?
           )",
    )
    .bind(meeting_id)
    .bind(meeting_id)
    .bind(meeting_id)
    .bind(NOTES_UNDO_LIMIT - 1)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE meeting_notes SET summary = ?, decisions = ?, action_items = ?,
                follow_ups = ?, open_questions = ?, edited = 1
         WHERE meeting_id = ?",
    )
    .bind(&text.summary)
    .bind(&text.decisions)
    .bind(&text.action_items)
    .bind(&text.follow_ups)
    .bind(&text.open_questions)
    .bind(meeting_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
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
                    stt_engine_id, llm_engine_id, error,
                    COALESCE(title_source, 'llm') AS title_source
             FROM meetings ORDER BY started_at DESC LIMIT ?",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Meetings whose title or notes contain `query`, newest first.
    ///
    /// Notes means the summary, decisions, action items, follow-ups and open
    /// questions. Matching is case-insensitive for ASCII (SQLite's `LIKE`),
    /// which is the accepted limit: a full-text index would need a virtual
    /// table and backfill for a list that rarely passes a few hundred rows.
    /// A blank query is the plain list.
    pub async fn search(&self, query: &str, limit: i64) -> Result<Vec<Meeting>, KeaError> {
        let query = query.trim();
        if query.is_empty() {
            return self.list(limit).await;
        }
        let pattern = format!("%{}%", escape_like(query));
        let rows = sqlx::query_as::<_, Meeting>(
            r"SELECT m.id, m.title, m.started_at, m.ended_at, m.status, m.capture_mode,
                     m.stt_engine_id, m.llm_engine_id, m.error,
                     COALESCE(m.title_source, 'llm') AS title_source
              FROM meetings m LEFT JOIN meeting_notes n ON n.meeting_id = m.id
              WHERE m.title LIKE ? ESCAPE '\'
                 OR n.summary LIKE ? ESCAPE '\'
                 OR n.decisions LIKE ? ESCAPE '\'
                 OR n.action_items LIKE ? ESCAPE '\'
                 OR n.follow_ups LIKE ? ESCAPE '\'
                 OR n.open_questions LIKE ? ESCAPE '\'
              ORDER BY m.started_at DESC LIMIT ?",
        )
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .bind(&pattern)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn get(&self, id: &str) -> Result<Option<MeetingDetail>, KeaError> {
        let meeting = sqlx::query_as::<_, Meeting>(
            "SELECT id, title, started_at, ended_at, status, capture_mode,
                    stt_engine_id, llm_engine_id, error,
                    COALESCE(title_source, 'llm') AS title_source
             FROM meetings WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(meeting) = meeting else {
            return Ok(None);
        };

        let segments = sqlx::query_as::<_, MeetingSegment>(
            "SELECT id, meeting_id, sequence, start_offset_ms, end_offset_ms, text, speaker_key
             FROM meeting_segments WHERE meeting_id = ? ORDER BY sequence ASC",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?;

        let speakers = self.speakers(id).await?;

        let notes = sqlx::query_as::<_, MeetingNotes>(NOTES_SELECT)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;

        let action_items = self.action_items(id).await?;

        let undo_depth: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM meeting_notes_versions WHERE meeting_id = ?")
                .bind(id)
                .fetch_one(&self.pool)
                .await?;

        Ok(Some(MeetingDetail {
            meeting,
            segments,
            notes,
            speakers,
            action_items,
            can_undo: undo_depth > 0,
        }))
    }

    pub async fn speakers(&self, meeting_id: &str) -> Result<Vec<MeetingSpeaker>, KeaError> {
        let rows = sqlx::query_as::<_, MeetingSpeaker>(
            "SELECT meeting_id, speaker_key, display_name, source
             FROM meeting_speakers WHERE meeting_id = ? ORDER BY speaker_key ASC",
        )
        .bind(meeting_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    /// Record the default name for one side of a meeting, leaving any existing
    /// row alone.
    ///
    /// `DO NOTHING` rather than an upsert: this runs when a meeting starts, and
    /// a meeting restarted or re-attributed must not clobber a name the user
    /// has since typed.
    pub async fn ensure_speaker(
        &self,
        meeting_id: &str,
        channel: SpeakerChannel,
        display_name: &str,
    ) -> Result<(), KeaError> {
        self.ensure_participant(meeting_id, channel.as_str(), display_name)
            .await
    }

    /// Record the default name for a participant key, leaving any existing row
    /// alone. The keyed form of [`MeetingRepo::ensure_speaker`], for voices that
    /// are not one of the two channels (`spk0`, `spk1`, …).
    ///
    /// Participant rows appear lazily, when a voice first shows up, so this is
    /// the call that creates them mid-meeting.
    pub async fn ensure_participant(
        &self,
        meeting_id: &str,
        key: &str,
        display_name: &str,
    ) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO meeting_speakers(meeting_id, speaker_key, display_name, source)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(meeting_id, speaker_key) DO NOTHING",
        )
        .bind(meeting_id)
        .bind(key)
        .bind(display_name)
        .bind(SpeakerSource::Channel.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Rename one side of a meeting. Always wins over the default, and marks
    /// the row `user` so nothing written by attribution overwrites it later.
    ///
    /// Action-item owners that quoted the old name are carried over in the same
    /// transaction; see [`MeetingRepo::rename_speaker`].
    pub async fn set_speaker_name(
        &self,
        meeting_id: &str,
        speaker_key: &str,
        display_name: &str,
    ) -> Result<(), KeaError> {
        let mut tx = self.pool.begin().await?;
        let old = speaker_name_in(&mut tx, meeting_id, speaker_key).await?;
        sqlx::query(
            "INSERT INTO meeting_speakers(meeting_id, speaker_key, display_name, source)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(meeting_id, speaker_key) DO UPDATE SET
                 display_name = excluded.display_name,
                 source = excluded.source",
        )
        .bind(meeting_id)
        .bind(speaker_key)
        .bind(display_name)
        .bind(SpeakerSource::User.as_str())
        .execute(&mut *tx)
        .await?;
        if let Some(old) = old.filter(|old| old.as_str() != display_name) {
            carry_owner_rename(&mut tx, meeting_id, &old, display_name).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Set a name the LLM proposed at stop. Writes only where the row is not
    /// `user`, so a name the user typed is never replaced by inference.
    ///
    /// The `WHERE` on the upsert is the guard, and `rows_affected` tells the
    /// caller whether it fired. When nothing was written there is nothing to
    /// carry over, so owners and prose are left alone.
    pub async fn set_inferred_speaker_name(
        &self,
        meeting_id: &str,
        speaker_key: &str,
        display_name: &str,
    ) -> Result<(), KeaError> {
        let mut tx = self.pool.begin().await?;
        let old = speaker_name_in(&mut tx, meeting_id, speaker_key).await?;
        let written = sqlx::query(
            "INSERT INTO meeting_speakers(meeting_id, speaker_key, display_name, source)
             VALUES (?, ?, ?, ?)
             ON CONFLICT(meeting_id, speaker_key) DO UPDATE SET
                 display_name = excluded.display_name,
                 source = excluded.source
             WHERE meeting_speakers.source != 'user'",
        )
        .bind(meeting_id)
        .bind(speaker_key)
        .bind(display_name)
        .bind(SpeakerSource::Inferred.as_str())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if written > 0 {
            if let Some(old) = old.filter(|old| old.as_str() != display_name) {
                carry_owner_rename(&mut tx, meeting_id, &old, display_name).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Rewrite the participant rows and segment keys after the stop-time
    /// re-cluster, in one transaction.
    ///
    /// - `relabels` gives the key each segment carries after the pass, by
    ///   `sequence`. Only segments that change are written. Keys are not
    ///   assumed to map one-to-one, because a voice split across two live keys
    ///   can end up as one, and one live key can split into two.
    /// - Participant rows for keys that no segment carries any more are
    ///   deleted. A user-typed name on such a row is moved onto the key that
    ///   most of its segments went to, so the person keeps the name the user
    ///   gave them. If that key already has a user name, the user's existing
    ///   name is kept and the orphan's is dropped.
    /// - `new_names` gives each surviving key its default display name, which is
    ///   how renumbering to "Person N" by first appearance reaches the row. Only
    ///   `channel` rows are renamed, and a missing row is created.
    pub async fn remap_segment_speakers(
        &self,
        meeting_id: &str,
        relabels: &[SegmentRelabel],
        new_names: &[(String, String)],
    ) -> Result<(), KeaError> {
        let mut tx = self.pool.begin().await?;

        let before: Vec<(i32, Option<String>)> = sqlx::query_as(
            "SELECT sequence, speaker_key FROM meeting_segments WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_all(&mut *tx)
        .await?;
        let rows = sqlx::query_as::<_, MeetingSpeaker>(
            "SELECT meeting_id, speaker_key, display_name, source
             FROM meeting_speakers WHERE meeting_id = ?",
        )
        .bind(meeting_id)
        .fetch_all(&mut *tx)
        .await?;

        let mut after: HashMap<i32, Option<String>> = before.iter().cloned().collect();
        for relabel in relabels {
            let key = Some(relabel.speaker_key.clone());
            if let Some(slot) = after.get_mut(&relabel.sequence) {
                if *slot != key {
                    sqlx::query(
                        "UPDATE meeting_segments SET speaker_key = ?
                         WHERE meeting_id = ? AND sequence = ?",
                    )
                    .bind(&relabel.speaker_key)
                    .bind(meeting_id)
                    .bind(relabel.sequence)
                    .execute(&mut *tx)
                    .await?;
                    *slot = key;
                }
            }
        }

        let live: HashSet<String> = after.values().flatten().cloned().collect();
        let mut user_keys: HashSet<String> = rows
            .iter()
            .filter(|row| row.source == SpeakerSource::User)
            .map(|row| row.speaker_key.clone())
            .collect();

        for orphan in rows.iter().filter(|row| !live.contains(&row.speaker_key)) {
            if orphan.source == SpeakerSource::User {
                // A user name with no segments left and nowhere to go is kept
                // rather than silently dropped.
                let Some(target) = majority_target(&before, &after, &orphan.speaker_key) else {
                    continue;
                };
                if !user_keys.contains(&target) {
                    sqlx::query(
                        "INSERT INTO meeting_speakers(meeting_id, speaker_key, display_name, source)
                         VALUES (?, ?, ?, ?)
                         ON CONFLICT(meeting_id, speaker_key) DO UPDATE SET
                             display_name = excluded.display_name,
                             source = excluded.source",
                    )
                    .bind(meeting_id)
                    .bind(&target)
                    .bind(&orphan.display_name)
                    .bind(SpeakerSource::User.as_str())
                    .execute(&mut *tx)
                    .await?;
                    user_keys.insert(target);
                }
            }
            sqlx::query("DELETE FROM meeting_speakers WHERE meeting_id = ? AND speaker_key = ?")
                .bind(meeting_id)
                .bind(&orphan.speaker_key)
                .execute(&mut *tx)
                .await?;
        }

        for (key, name) in new_names {
            if !live.contains(key) {
                continue;
            }
            sqlx::query(
                "INSERT INTO meeting_speakers(meeting_id, speaker_key, display_name, source)
                 VALUES (?, ?, ?, ?)
                 ON CONFLICT(meeting_id, speaker_key) DO UPDATE SET
                     display_name = excluded.display_name
                 WHERE meeting_speakers.source = 'channel'",
            )
            .bind(meeting_id)
            .bind(key)
            .bind(name)
            .bind(SpeakerSource::Channel.as_str())
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(())
    }

    pub async fn append_segment(
        &self,
        meeting_id: &str,
        seg: &NewSegment,
    ) -> Result<i64, KeaError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO meeting_segments(
                meeting_id, sequence, start_offset_ms, end_offset_ms, text, speaker_key
            ) VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(meeting_id)
        .bind(seg.sequence)
        .bind(seg.start_offset_ms)
        .bind(seg.end_offset_ms)
        .bind(&seg.text)
        .bind(seg.speaker_key.as_deref())
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    /// Writes generated notes. Generation is not a user change, so this clears
    /// the `edited` mark and pushes nothing onto the undo stack; `notes.edited`
    /// is ignored here.
    pub async fn upsert_notes(&self, notes: &MeetingNotes) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO meeting_notes(
                meeting_id, summary, decisions, action_items, follow_ups, open_questions,
                prompt_version, engine_id, model, edited
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 0)
            ON CONFLICT(meeting_id) DO UPDATE SET
                summary = excluded.summary,
                decisions = excluded.decisions,
                action_items = excluded.action_items,
                follow_ups = excluded.follow_ups,
                open_questions = excluded.open_questions,
                prompt_version = excluded.prompt_version,
                engine_id = excluded.engine_id,
                model = excluded.model,
                edited = 0",
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

    /// Replaces the notes with a user's edit, keeping what they replaced so
    /// [`Self::undo_notes`] can restore it.
    ///
    /// The current notes, with their `edited` mark, go onto the undo stack in
    /// the same transaction as the write, so a change is never stored without
    /// its undo. The stack is capped at [`NOTES_UNDO_LIMIT`]. Fails with
    /// `NotFound` when the meeting has no notes, since there is nothing to
    /// undo back to. `prompt_version`, engine and model are left as generated.
    pub async fn apply_user_notes(
        &self,
        meeting_id: &str,
        text: &NotesText,
    ) -> Result<(), KeaError> {
        let mut tx = self.pool.begin().await?;
        write_user_notes(&mut tx, meeting_id, text).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Applies a pending chat proposal to the notes, in one transaction with
    /// the proposal's move to `Applied`.
    ///
    /// The action-item prose is re-rendered from the rows here, not taken from
    /// the proposal: the rows are the source of truth and the proposal never
    /// carries them. A meeting with no rows keeps the notes' current prose, the
    /// same fallback the generated notes use, so a meeting recorded before the
    /// table does not lose its list.
    ///
    /// The whole thing is one transaction so a proposal is never half applied:
    /// the state check, the version push, the notes write and the state write
    /// commit together or not at all. Applying a proposal that is not pending is
    /// refused before anything is written, so a second apply pushes no second
    /// version.
    pub async fn apply_proposal(&self, meeting_id: &str, message_id: i64) -> Result<(), KeaError> {
        let mut tx = self.pool.begin().await?;

        let stored: Option<Option<String>> = sqlx::query_scalar(
            "SELECT m.meta FROM messages m JOIN conversations c ON c.id = m.conversation_id
             WHERE m.id = ? AND c.meeting_id = ?",
        )
        .bind(message_id)
        .bind(meeting_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(old_meta) = stored else {
            return Err(KeaError::NotFound(format!("chat message {message_id}")));
        };
        let Some(old_json) = old_meta else {
            return Err(KeaError::Other(format!(
                "chat message {message_id} has no proposal"
            )));
        };
        let mut meta: ProposalMeta = serde_json::from_str(&old_json)?;
        if meta.state != ProposalState::Pending {
            return Err(KeaError::Other(format!(
                "proposal {message_id} is already {}",
                meta.state.as_str()
            )));
        }

        let current = sqlx::query_as::<_, MeetingNotes>(NOTES_SELECT)
            .bind(meeting_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| KeaError::NotFound(format!("notes for meeting {meeting_id}")))?;

        // Dropped rows are left out, as they are in the prompt the model
        // answered from: the prose should describe the list the user sees as
        // open, not what they struck off.
        let rows = sqlx::query_as::<_, ActionItem>(
            "SELECT id, meeting_id, text, owner, due_hint, source_seq, status
             FROM meeting_action_items WHERE meeting_id = ? ORDER BY id ASC",
        )
        .bind(meeting_id)
        .fetch_all(&mut *tx)
        .await?;
        let parsed: Vec<ParsedActionItem> = rows
            .iter()
            .filter(|row| row.status != ActionItemStatus::Dropped)
            .map(|row| ParsedActionItem {
                text: row.text.clone(),
                owner: row.owner.clone(),
                due_hint: row.due_hint.clone(),
            })
            .collect();
        // Only a meeting with no rows at all keeps its prose: one whose rows
        // were all dropped renders to nothing, rather than resurrecting the
        // stale prose that still lists them.
        let action_items = if rows.is_empty() {
            current.action_items.clone()
        } else {
            render_action_items_prose(&parsed)
        };

        let changes =
            |section: &str| meta.changed.is_empty() || meta.changed.iter().any(|s| s == section);
        let pick = |section: &str, proposed: &str, now: &str| {
            if changes(section) {
                proposed.to_string()
            } else {
                now.to_string()
            }
        };
        let text = NotesText {
            summary: pick("summary", &meta.proposal.summary, &current.summary),
            decisions: pick("decisions", &meta.proposal.decisions, &current.decisions),
            action_items,
            follow_ups: pick("follow_ups", &meta.proposal.follow_ups, &current.follow_ups),
            open_questions: pick(
                "open_questions",
                &meta.proposal.open_questions,
                &current.open_questions,
            ),
        };
        write_user_notes(&mut tx, meeting_id, &text).await?;

        meta.state = ProposalState::Applied;
        let result = sqlx::query("UPDATE messages SET meta = ? WHERE id = ? AND meta = ?")
            .bind(serde_json::to_string(&meta)?)
            .bind(message_id)
            .bind(&old_json)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::Other(format!(
                "proposal {message_id} changed while it was being applied"
            )));
        }

        tx.commit().await?;
        Ok(())
    }

    /// Restores the newest version on the undo stack and returns the notes as
    /// they now stand, `edited` included. Returns `None` when the stack is
    /// empty, which is the state of notes that are still as generated. A
    /// restored generated version comes back with `edited = false`, so undoing
    /// every change clears the mark.
    pub async fn undo_notes(&self, meeting_id: &str) -> Result<Option<MeetingNotes>, KeaError> {
        let mut tx = self.pool.begin().await?;
        let newest: Option<(i64, String, i64)> = sqlx::query_as(
            "SELECT id, notes_json, edited FROM meeting_notes_versions
             WHERE meeting_id = ? ORDER BY id DESC LIMIT 1",
        )
        .bind(meeting_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some((version_id, notes_json, edited)) = newest else {
            return Ok(None);
        };

        let mut restored: MeetingNotes = serde_json::from_str(&notes_json)?;
        restored.edited = edited != 0;

        let result = sqlx::query(
            "UPDATE meeting_notes SET summary = ?, decisions = ?, action_items = ?,
                    follow_ups = ?, open_questions = ?, prompt_version = ?,
                    engine_id = ?, model = ?, edited = ?
             WHERE meeting_id = ?",
        )
        .bind(&restored.summary)
        .bind(&restored.decisions)
        .bind(&restored.action_items)
        .bind(&restored.follow_ups)
        .bind(&restored.open_questions)
        .bind(&restored.prompt_version)
        .bind(&restored.engine_id)
        .bind(&restored.model)
        .bind(i64::from(restored.edited))
        .bind(meeting_id)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::NotFound(format!(
                "notes for meeting {meeting_id}"
            )));
        }

        sqlx::query("DELETE FROM meeting_notes_versions WHERE id = ?")
            .bind(version_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(Some(restored))
    }

    /// The meeting's chat, oldest first. Empty when nothing has been asked yet,
    /// since the conversation is created on the first message.
    pub async fn chat_messages(
        &self,
        meeting_id: &str,
    ) -> Result<Vec<MeetingChatMessage>, KeaError> {
        let rows = sqlx::query_as::<_, ChatRow>(
            "SELECT m.id, m.role, m.content, m.meta, m.created_at
             FROM messages m JOIN conversations c ON c.id = m.conversation_id
             WHERE c.meeting_id = ? ORDER BY m.id",
        )
        .bind(meeting_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(ChatRow::into_message).collect()
    }

    /// Appends a message to the meeting's chat, creating the chat on the first
    /// one. The chat takes the meeting's LLM engine, so it reads back against
    /// the engine that answered. `proposal` is stored in `meta` and is only
    /// set on an assistant message. Fails with `NotFound` if the meeting does
    /// not exist.
    pub async fn append_chat_message(
        &self,
        meeting_id: &str,
        role: MessageRole,
        content: &str,
        proposal: Option<&ProposalMeta>,
    ) -> Result<i64, KeaError> {
        let mut tx = self.pool.begin().await?;
        let id = append_message_tx(&mut tx, meeting_id, role, content, proposal).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// One Ask AI exchange — the question and its reply — in one transaction,
    /// returning the reply's id. Written apart, a failure between the two left
    /// a question with no answer that every later turn then carried in its
    /// history.
    pub async fn append_chat_exchange(
        &self,
        meeting_id: &str,
        question: &str,
        reply: &str,
        proposal: Option<&ProposalMeta>,
    ) -> Result<i64, KeaError> {
        let mut tx = self.pool.begin().await?;
        append_message_tx(&mut tx, meeting_id, MessageRole::User, question, None).await?;
        let id =
            append_message_tx(&mut tx, meeting_id, MessageRole::Assistant, reply, proposal).await?;
        tx.commit().await?;
        Ok(id)
    }
}

async fn append_message_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    meeting_id: &str,
    role: MessageRole,
    content: &str,
    proposal: Option<&ProposalMeta>,
) -> Result<i64, KeaError> {
    let meta = proposal.map(serde_json::to_string).transpose()?;

    let existing: Option<i64> = sqlx::query_scalar(
        "SELECT id FROM conversations WHERE meeting_id = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(meeting_id)
    .fetch_optional(&mut **tx)
    .await?;
    let conversation_id = match existing {
        Some(id) => id,
        None => sqlx::query_scalar(
            "INSERT INTO conversations (feature_id, engine_id, meeting_id)
                 SELECT 'meetings', COALESCE(llm_engine_id, ''), id
                 FROM meetings WHERE id = ?
                 RETURNING id",
        )
        .bind(meeting_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| KeaError::NotFound(format!("meeting {meeting_id}")))?,
    };

    let message_id: i64 = sqlx::query_scalar(
        "INSERT INTO messages (conversation_id, role, content, meta)
             VALUES (?, ?, ?, ?) RETURNING id",
    )
    .bind(conversation_id)
    .bind(role.as_str())
    .bind(content)
    .bind(meta)
    .fetch_one(&mut **tx)
    .await?;
    Ok(message_id)
}

impl MeetingRepo {
    /// Deletes the meeting's chat and its messages. Clearing a chat that does
    /// not exist is not an error: the end state is the same.
    pub async fn clear_chat(&self, meeting_id: &str) -> Result<(), KeaError> {
        sqlx::query("DELETE FROM conversations WHERE meeting_id = ?")
            .bind(meeting_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Moves a proposal from `Pending` to `Applied` or `Discarded`. Any other
    /// move is refused, which is what keeps a double-clicked Apply from
    /// applying twice.
    ///
    /// The write is compare-and-set on the `meta` it read, so two racing
    /// callers cannot both leave `Pending`. Applying the notes is the caller's
    /// job and is not part of this transaction.
    pub async fn set_proposal_state(
        &self,
        meeting_id: &str,
        message_id: i64,
        to: ProposalState,
    ) -> Result<(), KeaError> {
        if to == ProposalState::Pending {
            return Err(KeaError::Other(
                "a proposal can only move out of pending".into(),
            ));
        }
        let stored: Option<Option<String>> = sqlx::query_scalar(
            "SELECT m.meta FROM messages m JOIN conversations c ON c.id = m.conversation_id
             WHERE m.id = ? AND c.meeting_id = ?",
        )
        .bind(message_id)
        .bind(meeting_id)
        .fetch_optional(&self.pool)
        .await?;
        let Some(old_meta) = stored else {
            return Err(KeaError::NotFound(format!("chat message {message_id}")));
        };
        let Some(old_json) = old_meta else {
            return Err(KeaError::Other(format!(
                "chat message {message_id} has no proposal"
            )));
        };
        let mut meta: ProposalMeta = serde_json::from_str(&old_json)?;
        if meta.state != ProposalState::Pending {
            return Err(KeaError::Other(format!(
                "proposal {message_id} is already {}",
                meta.state.as_str()
            )));
        }
        meta.state = to;
        let result = sqlx::query("UPDATE messages SET meta = ? WHERE id = ? AND meta = ?")
            .bind(serde_json::to_string(&meta)?)
            .bind(message_id)
            .bind(&old_json)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::Other(format!(
                "proposal {message_id} changed while it was being updated"
            )));
        }
        Ok(())
    }

    /// Rename a meeting, recording who named it.
    ///
    /// The `title_source` write is not decoration: it is what stops a
    /// re-synthesis overwriting a name a human typed, and what the "from
    /// Calendar" chip reads.
    pub async fn set_title_with_source(
        &self,
        id: &str,
        title: &str,
        source: TitleSource,
    ) -> Result<(), KeaError> {
        let result = sqlx::query("UPDATE meetings SET title = ?, title_source = ? WHERE id = ?")
            .bind(title)
            .bind(source.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::NotFound(format!("meeting {id}")));
        }
        Ok(())
    }

    /// Rename a meeting from the summariser. Kept as the one-argument spelling
    /// the stop path has always used; [`TitleSource::Llm`] is what it meant.
    pub async fn set_title(&self, id: &str, title: &str) -> Result<(), KeaError> {
        self.set_title_with_source(id, title, TitleSource::Llm)
            .await
    }

    /// Every action item on a meeting, oldest first, whatever its status.
    ///
    /// Ordered by `id` rather than by `status`: the checklist keeps its
    /// reading order when something is ticked off, instead of the row jumping
    /// out from under the cursor that ticked it.
    pub async fn action_items(&self, meeting_id: &str) -> Result<Vec<ActionItem>, KeaError> {
        let rows = sqlx::query_as::<_, ActionItem>(
            "SELECT id, meeting_id, text, owner, due_hint, source_seq, status
             FROM meeting_action_items WHERE meeting_id = ? ORDER BY id ASC",
        )
        .bind(meeting_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows)
    }

    pub async fn add_action_item(
        &self,
        meeting_id: &str,
        item: &NewActionItem,
    ) -> Result<i64, KeaError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO meeting_action_items(
                meeting_id, text, owner, due_hint, source_seq, status
            ) VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(meeting_id)
        .bind(&item.text)
        .bind(&item.owner)
        .bind(&item.due_hint)
        .bind(item.source_seq)
        .bind(ActionItemStatus::Open.as_str())
        .fetch_one(&self.pool)
        .await?;
        Ok(id)
    }

    pub async fn set_action_item_status(
        &self,
        id: i64,
        status: ActionItemStatus,
    ) -> Result<(), KeaError> {
        let result = sqlx::query("UPDATE meeting_action_items SET status = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(id)
            .execute(&self.pool)
            .await?;
        if result.rows_affected() == 0 {
            return Err(KeaError::NotFound(format!("action item {id}")));
        }
        Ok(())
    }

    /// Reconcile the table with a synthesis pass's action items and return the
    /// resulting rows.
    ///
    /// Each pass re-derives the whole list, so the latest pass is the source
    /// of truth for every row the model wrote and nobody has touched: such a
    /// row is reworded in place when the pass restated it, and deleted when
    /// the pass no longer lists it. Insert-only used to be the rule here, and
    /// it kept every rewording of every task — "Grant Confluence access" and
    /// "Grant Confluence access; it will take a while" became two rows, and a
    /// long meeting ended with each item four times over.
    ///
    /// Two kinds of row are the user's and are never rewritten or removed: a
    /// row that is no longer open (ticked off or dropped — deleting it would
    /// resurrect it as a fresh open row on the next pass) and a hand-added
    /// row, which is the one with no `source_seq`. An incoming item that
    /// matches one of those is skipped rather than added a second time.
    ///
    /// Matching is [`same_action`], so "Send the deck." and "send the deck"
    /// are one item, and so is a restatement that only adds detail.
    pub async fn reconcile_action_items(
        &self,
        meeting_id: &str,
        incoming: &[NewActionItem],
        source_seq: i64,
    ) -> Result<Vec<ActionItem>, KeaError> {
        let existing = self.action_items(meeting_id).await?;
        let existing_keys: Vec<String> = existing
            .iter()
            .map(|row| normalize_action_key(&row.text))
            .collect();
        let is_model_row =
            |row: &ActionItem| row.status == ActionItemStatus::Open && row.source_seq.is_some();

        let mut kept = vec![false; existing.len()];
        let mut seen: Vec<String> = Vec::new();
        let mut tx = self.pool.begin().await?;

        for item in incoming {
            let key = normalize_action_key(&item.text);
            // An empty key is a blank line the model emitted, not an item, and
            // a key already handled this pass is the model repeating itself.
            if key.is_empty() || seen.iter().any(|s| same_action(s, &key)) {
                continue;
            }
            seen.push(key.clone());

            let matched = existing_keys
                .iter()
                .enumerate()
                .find(|(i, k)| !kept[*i] && same_action(k, &key))
                .map(|(i, _)| i);

            match matched {
                Some(i) => {
                    kept[i] = true;
                    let row = &existing[i];
                    if is_model_row(row) {
                        sqlx::query(
                            "UPDATE meeting_action_items SET text = ?, owner = ?, due_hint = ?
                             WHERE id = ?",
                        )
                        .bind(&item.text)
                        .bind(&item.owner)
                        .bind(&item.due_hint)
                        .bind(row.id)
                        .execute(&mut *tx)
                        .await?;
                    }
                }
                None => {
                    sqlx::query(
                        "INSERT INTO meeting_action_items(
                            meeting_id, text, owner, due_hint, source_seq, status
                        ) VALUES (?, ?, ?, ?, ?, ?)",
                    )
                    .bind(meeting_id)
                    .bind(&item.text)
                    .bind(&item.owner)
                    .bind(&item.due_hint)
                    .bind(source_seq)
                    .bind(ActionItemStatus::Open.as_str())
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }

        for (row, kept) in existing.iter().zip(&kept) {
            if !kept && is_model_row(row) {
                sqlx::query("DELETE FROM meeting_action_items WHERE id = ?")
                    .bind(row.id)
                    .execute(&mut *tx)
                    .await?;
            }
        }

        tx.commit().await?;
        self.action_items(meeting_id).await
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
                    speaker_key: Some(SpeakerChannel::Local.as_str().into()),
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
            edited: false,
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

    #[tokio::test]
    async fn a_segments_speaker_key_survives_the_roundtrip() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = MeetingRepo::new(pool);
        repo.create(&NewMeeting {
            id: "m3".into(),
            title: "Sync".into(),
            capture_mode: CaptureMode::MicAndSystem,
            stt_engine_id: None,
            llm_engine_id: None,
        })
        .await
        .unwrap();

        for (sequence, speaker) in [
            (0, Some(SpeakerChannel::Local)),
            (1, Some(SpeakerChannel::Remote)),
            // Ambiguous, and stored as such rather than forced onto a side.
            (2, Some(SpeakerChannel::Mixed)),
            (3, None),
        ] {
            repo.append_segment(
                "m3",
                &NewSegment {
                    sequence,
                    start_offset_ms: sequence as i64 * 1000,
                    end_offset_ms: sequence as i64 * 1000 + 1000,
                    text: format!("line {sequence}"),
                    speaker_key: speaker.map(|c| c.as_str().to_string()),
                },
            )
            .await
            .unwrap();
        }

        let detail = repo.get("m3").await.unwrap().unwrap();
        let keys: Vec<Option<&str>> = detail
            .segments
            .iter()
            .map(|s| s.speaker_key.as_deref())
            .collect();
        assert_eq!(
            keys,
            vec![Some("local"), Some("remote"), Some("mixed"), None]
        );
    }

    /// A name the user typed must survive attribution seeding the defaults
    /// again — that is the whole reason `source` is stored.
    #[tokio::test]
    async fn a_user_named_speaker_is_not_overwritten_by_the_default() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = MeetingRepo::new(pool);
        repo.create(&NewMeeting {
            id: "m4".into(),
            title: "Sync".into(),
            capture_mode: CaptureMode::MicAndSystem,
            stt_engine_id: None,
            llm_engine_id: None,
        })
        .await
        .unwrap();

        repo.ensure_speaker("m4", SpeakerChannel::Local, "You")
            .await
            .unwrap();
        repo.ensure_speaker("m4", SpeakerChannel::Remote, "Others")
            .await
            .unwrap();
        repo.set_speaker_name("m4", "remote", "Priya")
            .await
            .unwrap();
        repo.ensure_speaker("m4", SpeakerChannel::Remote, "Others")
            .await
            .unwrap();

        let speakers = repo.speakers("m4").await.unwrap();
        assert_eq!(speakers.len(), 2);
        assert_eq!(speakers[0].speaker_key, "local");
        assert_eq!(speakers[0].display_name, "You");
        assert_eq!(speakers[0].source, SpeakerSource::Channel);
        assert_eq!(speakers[1].display_name, "Priya");
        assert_eq!(speakers[1].source, SpeakerSource::User);

        let detail = repo.get("m4").await.unwrap().unwrap();
        assert_eq!(detail.speakers.len(), 2);
    }

    #[tokio::test]
    async fn deleting_a_meeting_takes_its_speakers_with_it() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = MeetingRepo::new(pool.clone());
        repo.create(&NewMeeting {
            id: "m5".into(),
            title: "Sync".into(),
            capture_mode: CaptureMode::MicAndSystem,
            stt_engine_id: None,
            llm_engine_id: None,
        })
        .await
        .unwrap();
        repo.ensure_speaker("m5", SpeakerChannel::Local, "You")
            .await
            .unwrap();
        repo.delete("m5").await.unwrap();

        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meeting_speakers")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    /// A second call must not overwrite a name the user typed in between. The
    /// re-attribution path calls this for every participant it sees again.
    #[tokio::test]
    async fn a_second_ensure_participant_keeps_a_typed_name() {
        let (_pool, repo) = seeded("m11").await;
        repo.ensure_participant("m11", "spk1", "Person 2")
            .await
            .unwrap();
        repo.set_speaker_name("m11", "spk1", "Dan").await.unwrap();
        repo.ensure_participant("m11", "spk1", "Person 2")
            .await
            .unwrap();

        let speakers = repo.speakers("m11").await.unwrap();
        assert_eq!(speakers.len(), 1);
        assert_eq!(speakers[0].display_name, "Dan");
        assert_eq!(speakers[0].source, SpeakerSource::User);
    }

    /// Inference fills a default, and a name the user typed is never replaced
    /// by it. A later user rename always wins over an earlier inference.
    #[tokio::test]
    async fn an_inferred_name_never_replaces_a_typed_one() {
        let (_pool, repo) = seeded("m10").await;
        repo.ensure_participant("m10", "spk0", "Person 1")
            .await
            .unwrap();

        repo.set_inferred_speaker_name("m10", "spk0", "Dan")
            .await
            .unwrap();
        let speakers = repo.speakers("m10").await.unwrap();
        assert_eq!(speakers[0].display_name, "Dan");
        assert_eq!(speakers[0].source, SpeakerSource::Inferred);

        repo.set_speaker_name("m10", "spk0", "Sam").await.unwrap();
        repo.set_inferred_speaker_name("m10", "spk0", "Samuel")
            .await
            .unwrap();
        let speakers = repo.speakers("m10").await.unwrap();
        assert_eq!(speakers[0].display_name, "Sam");
        assert_eq!(speakers[0].source, SpeakerSource::User);
    }

    /// Renaming a participant rewrites the owners that quoted the old name, in
    /// this meeting only, and re-renders the prose column to match.
    #[tokio::test]
    async fn a_rename_rewrites_owners_in_this_meeting_only() {
        let (_pool, repo) = seeded("m8").await;
        repo.create(&NewMeeting {
            id: "m9".into(),
            title: "Other".into(),
            capture_mode: CaptureMode::MicOnly,
            stt_engine_id: None,
            llm_engine_id: None,
        })
        .await
        .unwrap();
        repo.ensure_participant("m8", "spk1", "Person 2")
            .await
            .unwrap();
        for id in ["m8", "m9"] {
            repo.add_action_item(
                id,
                &NewActionItem {
                    text: "Send the deck".into(),
                    owner: Some("Person 2".into()),
                    due_hint: Some("by Friday".into()),
                    source_seq: None,
                },
            )
            .await
            .unwrap();
        }
        repo.upsert_notes(&MeetingNotes {
            meeting_id: "m8".into(),
            summary: String::new(),
            decisions: String::new(),
            action_items: "Send the deck — Person 2 (by Friday)".into(),
            follow_ups: String::new(),
            open_questions: String::new(),
            prompt_version: "test".into(),
            engine_id: None,
            model: None,
            edited: false,
        })
        .await
        .unwrap();

        repo.set_speaker_name("m8", "spk1", "Dan").await.unwrap();

        let here = repo.action_items("m8").await.unwrap();
        assert_eq!(here[0].owner.as_deref(), Some("Dan"));
        let there = repo.action_items("m9").await.unwrap();
        assert_eq!(there[0].owner.as_deref(), Some("Person 2"));
        let notes = repo.get("m8").await.unwrap().unwrap().notes.unwrap();
        assert_eq!(notes.action_items, "Send the deck — Dan (by Friday)");
    }

    /// Two live keys that are one person become one key. The emptied row is
    /// dropped, and the surviving row takes the renumbered default name.
    #[tokio::test]
    async fn a_split_voice_merges_into_one_participant_on_remap() {
        let (_pool, repo) = seeded("m6").await;
        repo.ensure_participant("m6", "spk1", "Person 2")
            .await
            .unwrap();
        repo.ensure_participant("m6", "spk3", "Person 4")
            .await
            .unwrap();
        for (sequence, key) in [(0, "spk1"), (1, "spk3"), (2, "spk1"), (3, "spk3")] {
            repo.append_segment(
                "m6",
                &NewSegment {
                    sequence,
                    start_offset_ms: sequence as i64 * 1000,
                    end_offset_ms: sequence as i64 * 1000 + 1000,
                    text: format!("line {sequence}"),
                    speaker_key: Some(key.into()),
                },
            )
            .await
            .unwrap();
        }

        repo.remap_segment_speakers(
            "m6",
            &[
                SegmentRelabel {
                    sequence: 1,
                    speaker_key: "spk1".into(),
                },
                SegmentRelabel {
                    sequence: 3,
                    speaker_key: "spk1".into(),
                },
            ],
            &[("spk1".into(), "Person 1".into())],
        )
        .await
        .unwrap();

        let detail = repo.get("m6").await.unwrap().unwrap();
        let keys: Vec<&str> = detail
            .segments
            .iter()
            .map(|s| s.speaker_key.as_deref().unwrap())
            .collect();
        assert_eq!(keys, vec!["spk1"; 4]);
        assert_eq!(detail.speakers.len(), 1);
        assert_eq!(detail.speakers[0].speaker_key, "spk1");
        assert_eq!(detail.speakers[0].display_name, "Person 1");
        assert_eq!(detail.speakers[0].source, SpeakerSource::Channel);
    }

    /// A user's name follows its voice when the pass folds that voice into
    /// another key, and the default name on the surviving key does not
    /// overwrite it.
    #[tokio::test]
    async fn a_user_name_is_carried_onto_the_key_its_segments_moved_to() {
        let (_pool, repo) = seeded("m7").await;
        repo.ensure_participant("m7", "spk0", "Person 1")
            .await
            .unwrap();
        repo.ensure_participant("m7", "spk2", "Person 3")
            .await
            .unwrap();
        repo.set_speaker_name("m7", "spk2", "Dan").await.unwrap();
        for (sequence, key) in [(0, "spk0"), (1, "spk2"), (2, "spk2")] {
            repo.append_segment(
                "m7",
                &NewSegment {
                    sequence,
                    start_offset_ms: sequence as i64 * 1000,
                    end_offset_ms: sequence as i64 * 1000 + 1000,
                    text: format!("line {sequence}"),
                    speaker_key: Some(key.into()),
                },
            )
            .await
            .unwrap();
        }

        repo.remap_segment_speakers(
            "m7",
            &[
                SegmentRelabel {
                    sequence: 1,
                    speaker_key: "spk0".into(),
                },
                SegmentRelabel {
                    sequence: 2,
                    speaker_key: "spk0".into(),
                },
            ],
            &[("spk0".into(), "Person 1".into())],
        )
        .await
        .unwrap();

        let speakers = repo.speakers("m7").await.unwrap();
        assert_eq!(speakers.len(), 1);
        assert_eq!(speakers[0].speaker_key, "spk0");
        assert_eq!(speakers[0].display_name, "Dan");
        assert_eq!(speakers[0].source, SpeakerSource::User);
    }

    /// When both keys were typed by the user, the surviving key keeps its own
    /// name and the other is dropped. Nothing the user typed is overwritten.
    #[tokio::test]
    async fn a_typed_name_on_the_surviving_key_is_not_replaced() {
        let (_pool, repo) = seeded("m12").await;
        repo.ensure_participant("m12", "spk0", "Person 1")
            .await
            .unwrap();
        repo.ensure_participant("m12", "spk2", "Person 3")
            .await
            .unwrap();
        repo.set_speaker_name("m12", "spk0", "Sam").await.unwrap();
        repo.set_speaker_name("m12", "spk2", "Dan").await.unwrap();
        for (sequence, key) in [(0, "spk0"), (1, "spk2")] {
            repo.append_segment(
                "m12",
                &NewSegment {
                    sequence,
                    start_offset_ms: sequence as i64 * 1000,
                    end_offset_ms: sequence as i64 * 1000 + 1000,
                    text: format!("line {sequence}"),
                    speaker_key: Some(key.into()),
                },
            )
            .await
            .unwrap();
        }

        repo.remap_segment_speakers(
            "m12",
            &[SegmentRelabel {
                sequence: 1,
                speaker_key: "spk0".into(),
            }],
            &[],
        )
        .await
        .unwrap();

        let speakers = repo.speakers("m12").await.unwrap();
        assert_eq!(speakers.len(), 1);
        assert_eq!(speakers[0].display_name, "Sam");
        assert_eq!(speakers[0].source, SpeakerSource::User);
    }

    async fn seeded(id: &str) -> (SqlitePool, MeetingRepo) {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = MeetingRepo::new(pool.clone());
        repo.create(&NewMeeting {
            id: id.into(),
            title: "Untitled Meeting".into(),
            capture_mode: CaptureMode::MicOnly,
            stt_engine_id: None,
            llm_engine_id: None,
        })
        .await
        .unwrap();
        (pool, repo)
    }

    #[test]
    fn title_source_and_action_status_keep_their_stored_spellings() {
        for (source, text) in [
            (TitleSource::Calendar, "calendar"),
            (TitleSource::Llm, "llm"),
            (TitleSource::User, "user"),
        ] {
            assert_eq!(source.as_str(), text);
            assert_eq!(TitleSource::from_str(text), Some(source));
            assert_eq!(
                serde_json::to_string(&source).unwrap(),
                format!("\"{text}\"")
            );
        }
        for (status, text) in [
            (ActionItemStatus::Open, "open"),
            (ActionItemStatus::Done, "done"),
            (ActionItemStatus::Dropped, "dropped"),
        ] {
            assert_eq!(status.as_str(), text);
            assert_eq!(ActionItemStatus::from_str(text), Some(status));
        }
        assert_eq!(TitleSource::from_str("psychic"), None);
    }

    /// The column is NULL on every row that existed before item 17, and those
    /// titles really were written by the summariser.
    #[tokio::test]
    async fn a_null_title_source_reads_as_llm() {
        let (_pool, repo) = seeded("m10").await;
        let detail = repo.get("m10").await.unwrap().unwrap();
        assert_eq!(detail.meeting.title_source, TitleSource::Llm);
    }

    #[tokio::test]
    async fn a_calendar_title_records_where_it_came_from() {
        let (_pool, repo) = seeded("m11").await;
        repo.set_title_with_source("m11", "Q3 Roadmap Review", TitleSource::Calendar)
            .await
            .unwrap();
        let detail = repo.get("m11").await.unwrap().unwrap();
        assert_eq!(detail.meeting.title, "Q3 Roadmap Review");
        assert_eq!(detail.meeting.title_source, TitleSource::Calendar);

        // The old one-argument spelling still means "the summariser named it".
        repo.set_title("m11", "Roadmap sync").await.unwrap();
        assert_eq!(
            repo.get("m11").await.unwrap().unwrap().meeting.title_source,
            TitleSource::Llm
        );
    }

    #[test]
    fn action_keys_ignore_what_a_model_varies_between_passes() {
        assert_eq!(normalize_action_key("- Send the deck."), "send the deck");
        assert_eq!(normalize_action_key("  send   the deck  "), "send the deck");
        assert_eq!(normalize_action_key("• Send the Deck!"), "send the deck");
        assert_eq!(normalize_action_key("   "), "");
    }

    #[test]
    fn a_restatement_that_adds_detail_is_the_same_action() {
        assert!(same_action(
            "grant confluence access",
            "grant confluence access"
        ));
        assert!(same_action(
            "grant confluence access",
            "grant confluence access; it will take a while to come through"
        ));
        assert!(same_action(
            "bookmark sonar, confluence, bitbucket, and jenkins, including the links the lead sent",
            "bookmark sonar, confluence, bitbucket, and jenkins"
        ));
        // A word boundary, not a character one.
        assert!(!same_action("send the deck", "send the decks"));
        assert!(!same_action("", "send the deck"));
        assert!(!same_action("book the room", "send the deck"));
    }

    fn item(text: &str) -> NewActionItem {
        NewActionItem {
            text: text.into(),
            ..Default::default()
        }
    }

    /// Every interim pass re-derives the whole list, rewording as it goes.
    /// The table must end each pass holding that list once, not every
    /// wording the meeting ever produced.
    #[tokio::test]
    async fn a_later_pass_rewords_in_place_instead_of_adding_a_copy() {
        let (_pool, repo) = seeded("m12").await;
        let first = repo
            .reconcile_action_items(
                "m12",
                &[
                    NewActionItem {
                        text: "Grant Confluence access".into(),
                        owner: Some("Onboarding lead".into()),
                        due_hint: Some("in progress now".into()),
                        source_seq: None,
                    },
                    item("Sync with Dan on the DevOps project"),
                ],
                3,
            )
            .await
            .unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].source_seq, Some(3));

        let second = repo
            .reconcile_action_items(
                "m12",
                &[
                    item("Grant Confluence access; it will take a while to come through"),
                    item("- sync with Dan on the DevOps project."),
                    item("Send the onboarding documentation"),
                    // The model repeating itself within one reply.
                    item("Send the onboarding documentation"),
                    // A blank line is not an item.
                    NewActionItem::default(),
                ],
                7,
            )
            .await
            .unwrap();
        assert_eq!(second.len(), 3);
        // Same row, newest wording, original position.
        assert_eq!(second[0].id, first[0].id);
        assert_eq!(
            second[0].text,
            "Grant Confluence access; it will take a while to come through"
        );
        assert_eq!(second[0].source_seq, Some(3));
        assert_eq!(second[1].id, first[1].id);
        assert_eq!(second[2].text, "Send the onboarding documentation");
        assert_eq!(second[2].source_seq, Some(7));
    }

    /// A model row the latest pass no longer lists was folded into another
    /// item or turned out not to be a task; keeping it is how duplicates pile
    /// up.
    #[tokio::test]
    async fn a_model_row_the_latest_pass_drops_is_removed() {
        let (_pool, repo) = seeded("m14").await;
        repo.reconcile_action_items(
            "m14",
            &[
                item("Put email address in the chat"),
                item("Send email address in chat for system permissions"),
            ],
            1,
        )
        .await
        .unwrap();
        let rows = repo
            .reconcile_action_items(
                "m14",
                &[item("Send email address in chat for system permissions")],
                2,
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].text,
            "Send email address in chat for system permissions"
        );
    }

    /// What the user has touched is theirs: a pass must neither resurrect a
    /// ticked-off item, nor reword it, nor delete a hand-added one.
    #[tokio::test]
    async fn a_pass_leaves_ticked_and_hand_added_rows_alone() {
        let (_pool, repo) = seeded("m15").await;
        let first = repo
            .reconcile_action_items("m15", &[item("Send the deck"), item("Book the room")], 1)
            .await
            .unwrap();
        repo.set_action_item_status(first[0].id, ActionItemStatus::Done)
            .await
            .unwrap();
        repo.set_action_item_status(first[1].id, ActionItemStatus::Dropped)
            .await
            .unwrap();
        repo.add_action_item("m15", &item("Call the venue"))
            .await
            .unwrap();

        let rows = repo
            .reconcile_action_items(
                "m15",
                &[item("Send the deck by Friday"), item("Draft the summary")],
                4,
            )
            .await
            .unwrap();
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Send the deck",
                "Book the room",
                "Call the venue",
                "Draft the summary"
            ]
        );
        assert_eq!(rows[0].status, ActionItemStatus::Done);
        assert_eq!(rows[1].status, ActionItemStatus::Dropped);
        assert_eq!(rows[2].source_seq, None);
    }

    #[tokio::test]
    async fn deleting_a_meeting_takes_its_action_items_with_it() {
        let (pool, repo) = seeded("m13").await;
        repo.add_action_item(
            "m13",
            &NewActionItem {
                text: "Ship it".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        repo.delete("m13").await.unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meeting_action_items")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    /// A seeded meeting with a title and generated notes, for the workspace
    /// tests. The notes are written with `edited: true` on purpose: generation
    /// has to clear that mark, so every test starts from generated notes.
    async fn with_generated_notes(
        id: &str,
        title: &str,
        summary: &str,
    ) -> (SqlitePool, MeetingRepo) {
        let (pool, repo) = seeded(id).await;
        repo.set_title(id, title).await.unwrap();
        repo.upsert_notes(&MeetingNotes {
            meeting_id: id.into(),
            summary: summary.into(),
            decisions: String::new(),
            action_items: String::new(),
            follow_ups: String::new(),
            open_questions: String::new(),
            prompt_version: "meeting-notes-v1".into(),
            engine_id: Some("openai".into()),
            model: Some("gpt-4o-mini".into()),
            edited: true,
        })
        .await
        .unwrap();
        (pool, repo)
    }

    fn text(summary: &str) -> NotesText {
        NotesText {
            summary: summary.into(),
            decisions: String::new(),
            action_items: String::new(),
            follow_ups: String::new(),
            open_questions: String::new(),
        }
    }

    fn proposal(summary: &str, state: ProposalState) -> ProposalMeta {
        ProposalMeta {
            proposal: text(summary),
            state,
            changed: vec!["summary".into()],
        }
    }

    /// Deleting a meeting removes its chat, its messages and its undo stack,
    /// which is what the cascade on each new table is for.
    #[tokio::test]
    async fn deleting_a_meeting_cascades_to_chat_and_notes_versions() {
        let (pool, repo) = with_generated_notes("m20", "Planning", "Generated").await;
        repo.apply_user_notes("m20", &text("Edited")).await.unwrap();
        repo.append_chat_message("m20", MessageRole::User, "hi", None)
            .await
            .unwrap();

        repo.delete("m20").await.unwrap();

        for table in [
            "conversations",
            "messages",
            "meeting_notes_versions",
            "meeting_notes",
        ] {
            let left: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
                .fetch_one(&pool)
                .await
                .unwrap();
            assert_eq!(left, 0, "{table} should be empty after the cascade");
        }
    }

    #[tokio::test]
    async fn search_matches_a_title() {
        let (_pool, repo) = with_generated_notes("s1", "Quarterly planning", "Budget talk").await;
        let hits = repo.search("planning", 20).await.unwrap();
        assert_eq!(
            hits.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["s1"]
        );
    }

    /// The title says nothing about Confluence; only the summary does.
    #[tokio::test]
    async fn search_matches_a_summary_only_word() {
        let (_pool, repo) = with_generated_notes("s2", "Standup", "We picked Confluence").await;
        let hits = repo.search("confluence", 20).await.unwrap();
        assert_eq!(
            hits.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["s2"]
        );
    }

    #[tokio::test]
    async fn search_is_case_insensitive() {
        let (_pool, repo) = with_generated_notes("s3", "Standup", "We picked Confluence").await;
        assert_eq!(repo.search("CONFLUENCE", 20).await.unwrap().len(), 1);
        assert_eq!(repo.search("confluence", 20).await.unwrap().len(), 1);
    }

    /// A bare `%` is a wildcard in `LIKE`. Escaped, it only matches a literal
    /// percent sign, so the meeting without one must not come back.
    #[tokio::test]
    async fn search_treats_percent_literally() {
        let (_pool, repo) = with_generated_notes("s4", "Revenue 100% up", "").await;
        repo.create(&NewMeeting {
            id: "s5".into(),
            title: "Revenue 100 up".into(),
            capture_mode: CaptureMode::MicOnly,
            stt_engine_id: None,
            llm_engine_id: None,
        })
        .await
        .unwrap();
        let hits = repo.search("%", 20).await.unwrap();
        assert_eq!(
            hits.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["s4"]
        );
    }

    #[tokio::test]
    async fn search_treats_underscore_literally() {
        let (_pool, repo) = with_generated_notes("s6", "snake_case review", "").await;
        repo.create(&NewMeeting {
            id: "s8".into(),
            title: "snakeXcase review".into(),
            capture_mode: CaptureMode::MicOnly,
            stt_engine_id: None,
            llm_engine_id: None,
        })
        .await
        .unwrap();
        let hits = repo.search("_", 20).await.unwrap();
        assert_eq!(
            hits.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["s6"]
        );
    }

    #[tokio::test]
    async fn a_blank_search_is_the_plain_list() {
        let (_pool, repo) = with_generated_notes("s7", "Anything", "").await;
        assert_eq!(repo.search("   ", 20).await.unwrap().len(), 1);
    }

    /// Generation writes the notes and clears the edited mark without a version,
    /// so the first thing a user can undo is their own change.
    #[tokio::test]
    async fn generation_clears_the_edited_mark_and_pushes_no_version() {
        let (_pool, repo) = with_generated_notes("u1", "Sync", "Generated").await;
        let detail = repo.get("u1").await.unwrap().unwrap();
        assert!(!detail.notes.unwrap().edited);
        assert!(!detail.can_undo);
    }

    #[tokio::test]
    async fn apply_then_undo_restores_the_generated_notes_and_clears_edited() {
        let (_pool, repo) = with_generated_notes("u2", "Sync", "Generated").await;

        repo.apply_user_notes("u2", &text("Shorter")).await.unwrap();
        let edited = repo.get("u2").await.unwrap().unwrap();
        assert!(edited.notes.as_ref().unwrap().edited);
        assert_eq!(edited.notes.unwrap().summary, "Shorter");
        assert!(edited.can_undo);

        let restored = repo.undo_notes("u2").await.unwrap().unwrap();
        assert_eq!(restored.summary, "Generated");
        assert!(!restored.edited);

        let after = repo.get("u2").await.unwrap().unwrap();
        assert!(!after.notes.unwrap().edited);
        assert!(!after.can_undo);
    }

    #[tokio::test]
    async fn undo_steps_back_one_change_at_a_time() {
        let (_pool, repo) = with_generated_notes("u3", "Sync", "Generated").await;
        repo.apply_user_notes("u3", &text("First")).await.unwrap();
        repo.apply_user_notes("u3", &text("Second")).await.unwrap();

        assert_eq!(
            repo.undo_notes("u3").await.unwrap().unwrap().summary,
            "First"
        );
        assert_eq!(
            repo.undo_notes("u3").await.unwrap().unwrap().summary,
            "Generated"
        );
        assert!(repo.undo_notes("u3").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn undo_with_an_empty_stack_changes_nothing() {
        let (_pool, repo) = with_generated_notes("u4", "Sync", "Generated").await;
        assert!(repo.undo_notes("u4").await.unwrap().is_none());
        let detail = repo.get("u4").await.unwrap().unwrap();
        assert_eq!(detail.notes.unwrap().summary, "Generated");
    }

    /// Applying with no notes row has nothing to restore to, so it is refused
    /// rather than storing a change without its undo.
    #[tokio::test]
    async fn applying_to_a_meeting_without_notes_is_refused() {
        let (_pool, repo) = seeded("u5").await;
        let err = repo.apply_user_notes("u5", &text("x")).await.unwrap_err();
        assert!(matches!(err, KeaError::NotFound(_)));
    }

    /// Past the cap the oldest version goes, so the stack walks back through
    /// fifty edits and stops at the first one, not at the generated notes.
    #[tokio::test]
    async fn the_undo_stack_is_capped_but_keeps_the_generated_notes() {
        let (pool, repo) = with_generated_notes("u6", "Sync", "Generated").await;
        for n in 1..=(NOTES_UNDO_LIMIT + 1) {
            repo.apply_user_notes("u6", &text(&format!("edit {n}")))
                .await
                .unwrap();
        }
        let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meeting_notes_versions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, NOTES_UNDO_LIMIT);

        // The oldest *edits* went; the generated notes at the bottom did not,
        // so undoing everything still lands on them with the mark cleared.
        let mut last = None;
        for _ in 0..NOTES_UNDO_LIMIT {
            last = repo.undo_notes("u6").await.unwrap();
        }
        let last = last.unwrap();
        assert_eq!(last.summary, "Generated");
        assert!(!last.edited);
        assert!(repo.undo_notes("u6").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_meeting_has_no_chat_until_the_first_message() {
        let (pool, repo) = seeded("c1").await;
        assert!(repo.chat_messages("c1").await.unwrap().is_empty());
        let conversations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM conversations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(conversations, 0);
    }

    #[tokio::test]
    async fn chat_messages_round_trip_with_their_proposal() {
        let (_pool, repo) = seeded("c2").await;
        repo.append_chat_message("c2", MessageRole::User, "shorter?", None)
            .await
            .unwrap();
        let meta = proposal("Three bullets", ProposalState::Pending);
        repo.append_chat_message("c2", MessageRole::Assistant, "Here.", Some(&meta))
            .await
            .unwrap();

        let chat = repo.chat_messages("c2").await.unwrap();
        assert_eq!(chat.len(), 2);
        assert_eq!(chat[0].role, MessageRole::User);
        assert_eq!(chat[0].proposal, None);
        assert_eq!(chat[1].role, MessageRole::Assistant);
        assert_eq!(chat[1].proposal, Some(meta));
    }

    #[tokio::test]
    async fn a_chat_message_for_a_missing_meeting_is_refused() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = MeetingRepo::new(pool);
        let err = repo
            .append_chat_message("nope", MessageRole::User, "hi", None)
            .await
            .unwrap_err();
        assert!(matches!(err, KeaError::NotFound(_)));
    }

    #[tokio::test]
    async fn a_proposal_moves_from_pending_to_applied_once() {
        let (_pool, repo) = seeded("c3").await;
        let id = repo
            .append_chat_message(
                "c3",
                MessageRole::Assistant,
                "Here.",
                Some(&proposal("Short", ProposalState::Pending)),
            )
            .await
            .unwrap();

        repo.set_proposal_state("c3", id, ProposalState::Applied)
            .await
            .unwrap();
        let state = repo.chat_messages("c3").await.unwrap()[0]
            .proposal
            .as_ref()
            .unwrap()
            .state;
        assert_eq!(state, ProposalState::Applied);

        let again = repo
            .set_proposal_state("c3", id, ProposalState::Applied)
            .await
            .unwrap_err();
        assert!(matches!(again, KeaError::Other(_)));
        let switch = repo
            .set_proposal_state("c3", id, ProposalState::Discarded)
            .await
            .unwrap_err();
        assert!(matches!(switch, KeaError::Other(_)));
    }

    #[tokio::test]
    async fn a_proposal_moves_from_pending_to_discarded_once() {
        let (_pool, repo) = seeded("c4").await;
        let id = repo
            .append_chat_message(
                "c4",
                MessageRole::Assistant,
                "Here.",
                Some(&proposal("Short", ProposalState::Pending)),
            )
            .await
            .unwrap();

        repo.set_proposal_state("c4", id, ProposalState::Discarded)
            .await
            .unwrap();
        let state = repo.chat_messages("c4").await.unwrap()[0]
            .proposal
            .as_ref()
            .unwrap()
            .state;
        assert_eq!(state, ProposalState::Discarded);

        let applied = repo
            .set_proposal_state("c4", id, ProposalState::Applied)
            .await
            .unwrap_err();
        assert!(matches!(applied, KeaError::Other(_)));
    }

    #[tokio::test]
    async fn a_proposal_cannot_go_back_to_pending_or_belong_to_another_meeting() {
        let (_pool, repo) = seeded("c5").await;
        repo.create(&NewMeeting {
            id: "c6".into(),
            title: "Other".into(),
            capture_mode: CaptureMode::MicOnly,
            stt_engine_id: None,
            llm_engine_id: None,
        })
        .await
        .unwrap();
        let id = repo
            .append_chat_message(
                "c5",
                MessageRole::Assistant,
                "Here.",
                Some(&proposal("Short", ProposalState::Pending)),
            )
            .await
            .unwrap();

        let back = repo
            .set_proposal_state("c5", id, ProposalState::Pending)
            .await
            .unwrap_err();
        assert!(matches!(back, KeaError::Other(_)));
        let wrong = repo
            .set_proposal_state("c6", id, ProposalState::Applied)
            .await
            .unwrap_err();
        assert!(matches!(wrong, KeaError::NotFound(_)));
    }

    #[tokio::test]
    async fn clearing_a_chat_removes_it_and_a_new_message_starts_a_fresh_one() {
        let (_pool, repo) = seeded("c7").await;
        repo.append_chat_message("c7", MessageRole::User, "one", None)
            .await
            .unwrap();
        repo.clear_chat("c7").await.unwrap();
        assert!(repo.chat_messages("c7").await.unwrap().is_empty());
        repo.clear_chat("c7").await.unwrap();

        repo.append_chat_message("c7", MessageRole::User, "two", None)
            .await
            .unwrap();
        let chat = repo.chat_messages("c7").await.unwrap();
        assert_eq!(chat.len(), 1);
        assert_eq!(chat[0].content, "two");
    }

    /// A second Apply must be refused before it writes anything: no second
    /// version on the undo stack, and the notes stay as the first Apply left
    /// them. This is what makes a double click safe.
    #[tokio::test]
    async fn applying_a_proposal_twice_pushes_one_version() {
        let (pool, repo) = with_generated_notes("a1", "Sync", "Generated").await;
        let id = repo
            .append_chat_message(
                "a1",
                MessageRole::Assistant,
                "Shorter.",
                Some(&proposal("Short", ProposalState::Pending)),
            )
            .await
            .unwrap();

        repo.apply_proposal("a1", id).await.unwrap();
        let second = repo.apply_proposal("a1", id).await.unwrap_err();
        assert!(matches!(second, KeaError::Other(_)));

        let versions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meeting_notes_versions")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(versions, 1);
        let detail = repo.get("a1").await.unwrap().unwrap();
        assert_eq!(detail.notes.unwrap().summary, "Short");
    }

    #[tokio::test]
    async fn applying_a_proposal_marks_it_applied_and_the_notes_edited() {
        let (_pool, repo) = with_generated_notes("a2", "Sync", "Generated").await;
        let id = repo
            .append_chat_message(
                "a2",
                MessageRole::Assistant,
                "Shorter.",
                Some(&proposal("Short", ProposalState::Pending)),
            )
            .await
            .unwrap();

        repo.apply_proposal("a2", id).await.unwrap();

        let detail = repo.get("a2").await.unwrap().unwrap();
        let notes = detail.notes.unwrap();
        assert_eq!(notes.summary, "Short");
        assert!(notes.edited);
        assert!(detail.can_undo);
        let state = repo.chat_messages("a2").await.unwrap()[0]
            .proposal
            .as_ref()
            .unwrap()
            .state;
        assert_eq!(state, ProposalState::Applied);
    }

    /// The rows are the source of truth, so the action-item prose written by
    /// an apply is rendered from them, not copied from the proposal (which
    /// carries whatever prose it was made against).
    #[tokio::test]
    async fn applying_rerenders_the_action_items_from_the_rows() {
        let (_pool, repo) = with_generated_notes("a3", "Sync", "Generated").await;
        repo.add_action_item(
            "a3",
            &NewActionItem {
                text: "Send the deck".into(),
                owner: Some("Priya".into()),
                due_hint: Some("by Friday".into()),
                source_seq: None,
            },
        )
        .await
        .unwrap();
        let mut stale = proposal("Short", ProposalState::Pending);
        stale.proposal.action_items = "stale prose".into();
        let id = repo
            .append_chat_message("a3", MessageRole::Assistant, "Shorter.", Some(&stale))
            .await
            .unwrap();

        repo.apply_proposal("a3", id).await.unwrap();

        let notes = repo.get("a3").await.unwrap().unwrap().notes.unwrap();
        assert_eq!(notes.action_items, "Send the deck — Priya (by Friday)");
    }

    /// The reviewer's case: the user hand-edits decisions after a summary
    /// proposal arrived. Applying the proposal changes the summary only.
    #[tokio::test]
    async fn applying_a_proposal_keeps_a_later_hand_edit_to_another_section() {
        let (_pool, repo) = with_generated_notes("a5", "Sync", "Generated").await;
        let id = repo
            .append_chat_message(
                "a5",
                MessageRole::Assistant,
                "Shorter.",
                Some(&proposal("Short", ProposalState::Pending)),
            )
            .await
            .unwrap();
        repo.apply_user_notes(
            "a5",
            &NotesText {
                decisions: "Decided by hand".into(),
                ..text("Generated")
            },
        )
        .await
        .unwrap();

        repo.apply_proposal("a5", id).await.unwrap();

        let notes = repo.get("a5").await.unwrap().unwrap().notes.unwrap();
        assert_eq!(notes.summary, "Short");
        assert_eq!(notes.decisions, "Decided by hand");
    }

    /// Every row dropped is an empty list, not a cue to restore the old prose
    /// that still names them.
    #[tokio::test]
    async fn applying_with_every_row_dropped_writes_no_action_items() {
        let (_pool, repo) = with_generated_notes("a6", "Sync", "Generated").await;
        let row = repo
            .add_action_item(
                "a6",
                &NewActionItem {
                    text: "Send the deck".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        repo.set_action_item_status(row, ActionItemStatus::Dropped)
            .await
            .unwrap();
        let mut stale = proposal("Short", ProposalState::Pending);
        stale.proposal.action_items = "Send the deck".into();
        let id = repo
            .append_chat_message("a6", MessageRole::Assistant, "Shorter.", Some(&stale))
            .await
            .unwrap();

        repo.apply_proposal("a6", id).await.unwrap();

        let notes = repo.get("a6").await.unwrap().unwrap().notes.unwrap();
        assert_eq!(notes.action_items, "");
    }

    #[tokio::test]
    async fn an_exchange_stores_the_question_and_the_reply_together() {
        let (_pool, repo) = seeded("c8").await;
        let reply = repo
            .append_chat_exchange("c8", "What was decided?", "The launch slips.", None)
            .await
            .unwrap();
        let chat = repo.chat_messages("c8").await.unwrap();
        assert_eq!(chat.len(), 2);
        assert_eq!(chat[0].role, MessageRole::User);
        assert_eq!(chat[1].id, reply);
        assert_eq!(chat[1].content, "The launch slips.");
    }

    #[tokio::test]
    async fn a_discarded_proposal_cannot_be_applied() {
        let (_pool, repo) = with_generated_notes("a4", "Sync", "Generated").await;
        let id = repo
            .append_chat_message(
                "a4",
                MessageRole::Assistant,
                "Shorter.",
                Some(&proposal("Short", ProposalState::Pending)),
            )
            .await
            .unwrap();
        repo.set_proposal_state("a4", id, ProposalState::Discarded)
            .await
            .unwrap();

        assert!(repo.apply_proposal("a4", id).await.is_err());
        let notes = repo.get("a4").await.unwrap().unwrap().notes.unwrap();
        assert_eq!(notes.summary, "Generated");
        assert!(!notes.edited);
    }

    #[test]
    fn speaker_source_keeps_its_stored_spelling() {
        for (source, text) in [
            (SpeakerSource::Channel, "channel"),
            (SpeakerSource::User, "user"),
            (SpeakerSource::Inferred, "inferred"),
        ] {
            assert_eq!(source.as_str(), text);
            assert_eq!(SpeakerSource::from_str(text), Some(source));
            assert_eq!(
                serde_json::to_string(&source).unwrap(),
                format!("\"{text}\"")
            );
        }
        assert_eq!(SpeakerSource::from_str("robot"), None);
    }
}
