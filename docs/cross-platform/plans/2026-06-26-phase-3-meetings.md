# KEA Phase 3 — Meetings (Granola-like) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver Granola-like meeting capture on macOS first (Windows/Linux follow as parallel platform tasks): start a meeting → capture mic **and** system/loopback audio when available (D14) → segmented transcription via a user-bound `SttEngine` → on stop, AI synthesis (structured notes + title) via a user-bound `LlmEngine` → persist transcript segments and notes in `data.db` (D9 content boundary) → surface live transcript, level meter, and state through Tauri events and a React Meetings panel. **Mic-only capture must work end-to-end** when loopback is unavailable.

**Architecture:** Phase 0–2 shipped trait + registry scaffolding (`SttEngine::transcribe`, `LlmEngine::complete`, `SlotResolver::resolve_stt`/`resolve_llm`, `AudioIo` mic capture, `DictationFeature` + `run_dictation` as the orchestration template). Phase 3 extends `AudioIo` with meeting capture (`start_meeting` / `stop_meeting`, mixed mic+system), adds `Permissions` (Screen Recording on macOS), `data.db` meeting tables + `MeetingRepo`, `MeetingFeature` + `run_meeting()` with segmented transcription and LLM synthesis, and thin Tauri wiring. Consumers depend on traits; `src-tauri` is the only composition root.

**Tech Stack:** Rust (edition 2021), Tauri 2.x, `cpal` (mic + optional loopback device), `screencapturekit` (optional, macOS 13+ system audio — feature-gated), `sqlx`, `tokio`, `async-trait`, `serde`/`serde_json`, Vite + React + TypeScript (D10).

## Global Constraints

- **Product name:** `KEA` everywhere (`kea-*` crates, `ai.kea.app`). _(D13.)_
- **Plugin model:** internal trait + registry, compiled in. No dynamic loading. _(D1, D2.)_
- **Storage boundary (D9):** `config.db` = settings, bindings, hotkey bindings, provider config, meeting **settings** (segment duration, capture preference); `data.db` = actions **and meeting content** (transcripts, segments, synthesized notes — full text by default per §6.3); **keyring = credentials only**. DB rows store `engine_id`, `model`, `provider_ref` references — never secrets.
- **Web UI (D10):** React pages composed from a shared component library; Rust plugins expose typed Tauri commands/events only.
- **Async:** all engine/platform I/O trait methods are `async` (`async-trait`).
- **TDD:** every code task is test-first. Rust async tests use `#[tokio::test]`. Store tests use `sqlite::memory:`; feature orchestration uses `FakeAudioIo`, fake STT/LLM engines — **never hit real OpenAI, real mic, or real loopback in unit tests**.
- **No real audio in unit tests:** `AudioIo` meeting paths are exercised via `FakeMeetingAudioIo`; macOS `cpal` / ScreenCaptureKit impls are compile + manual acceptance only.
- **No real network in unit tests:** LLM synthesis tests inject `FakeLlmEngine` or parse prompts from `NoopLlmEngine`; no wiremock required for meeting orchestration (reuse existing engine fakes).
- **macOS-first:** Tasks through macOS meeting E2E must pass before treating Phase 3 done on the primary dev machine. Windows WASAPI loopback + Linux Pulse/PipeWire monitor impls are clearly labeled **parallel per-OS** and may land after macOS E2E.
- **Feature-gate risky system audio:** ScreenCaptureKit (`screencapturekit` crate) is behind `features = ["system-audio-sck"]` on `kea-platform`. Default `cargo test --workspace` and CI **do not** enable this feature. BlackHole/cpal loopback detection compiles in default builds but is best-effort (no extra native dep).
- **Mic-only fallback is mandatory:** `MeetingFeature` records with `capture_mode = "mic_only"` when `SystemAudioCapability::Unavailable` or permission denied; UI labels the mode clearly. Phase 3 is **not done** if mic-only path fails.
- **Targets:** code compiles on macOS, Windows, Linux; CI runs `cargo test --workspace` (without `--features system-audio-sck`) on all three.
- **Commits:** frequent conventional commits, one per task minimum. Use `git commit --no-verify` when the legacy Vox FluidAudio pre-commit hook blocks unrelated paths.

### System-audio capture decision (macOS)

| Priority | Approach | Crate / API | Gate | Feasibility |
|----------|----------|-------------|------|-------------|
| **1 (preferred loopback)** | ScreenCaptureKit system audio | [`screencapturekit`](https://crates.io/crates/screencapturekit) v7.x, features `macos_13_0` + `async` | `kea-platform` feature `system-audio-sck` | **Usable but gated.** Mature-ish bindings (666k+ downloads, active 2026 releases, audio examples). Requires macOS 13+ and **Screen Recording** permission. Callback-driven capture must be bridged to `PcmFrame` mpsc — integration risk is real (main-thread / dispatch-queue semantics), so ship behind feature flag + manual acceptance before enabling in default release builds. |
| **2 (best-effort)** | cpal input from virtual loopback device (e.g. user-installed BlackHole) | existing `cpal` | always compiled on macOS | **Opportunistic.** No extra permission if user routes system audio to a multi-output / aggregate device. Detect device by name (`"BlackHole"`, `"Loopback"`). Works only when user configured routing — document in UI. |
| **3 (guaranteed baseline)** | Mic only | existing `cpal` mic path | none | **Works today.** Meeting records and transcribes the local speaker; remote participants missing until loopback lands. |

**Honest assessment:** There is no fully mature, zero-risk Rust ScreenCaptureKit audio path — `screencapturekit` is the best available option, but Phase 3 **Definition of Done** is satisfied on **mic-only**; full D14 parity on macOS requires `--features system-audio-sck` + manual Screen Recording acceptance. **`cidre`** is documented as an alternative if `screencapturekit` blocks (lower-level, more integration work) — not planned unless Task 14 spikes fail.

**Deferred (parallel tasks):** Windows WASAPI loopback (`IAudioClient` loopback mode or cpal WASAPI loopback device); Linux PulseAudio/PipeWire monitor source via cpal device enumeration.

---

## File Structure

```
kea/
├─ Cargo.toml                              # optional screencapturekit workspace dep
├─ crates/
│  ├─ core/
│  │  ├─ migrations/
│  │  │  ├─ config/0004_meetings.sql       # meeting settings KV keys (comment migration)
│  │  │  └─ data/0004_meetings.sql         # meetings, meeting_segments, meeting_notes
│  │  └─ src/
│  │     ├─ meetings/
│  │     │  ├─ mod.rs
│  │     │  ├─ settings.rs                # MeetingSettings repo (config.db)
│  │     │  ├─ synthesis.rs               # prompt builders (parity w/ VoxNative MeetingSynthesis)
│  │     │  └─ segment.rs                 # chunk_pcm_by_duration (pure)
│  │     └─ store/
│  │        └─ meetings.rs                 # MeetingRepo (data.db)
│  ├─ features/
│  │  └─ src/
│  │     ├─ meetings.rs                    # MeetingFeature + run_meeting()
│  │     └─ lib.rs
│  └─ platform/
│     └─ src/
│        ├─ lib.rs                           # new_permissions()
│        ├─ permissions/
│        │  ├─ mod.rs                        # Permissions trait, PermKind
│        │  ├─ macos.rs                      # CGPreflightScreenCaptureAccess
│        │  └─ stub.rs
│        └─ audio/
│           ├─ mod.rs                        # extend AudioIo for meetings
│           ├─ util.rs                       # mix_frames, chunk_pcm_by_duration
│           ├─ macos.rs                      # meeting capture (mic + optional loopback)
│           ├─ macos_sck.rs                  # [feature system-audio-sck] ScreenCaptureKit
│           ├─ windows_loopback.rs           # [PARALLEL] WASAPI loopback
│           └─ linux_monitor.rs            # [PARALLEL] Pulse/PipeWire monitor
├─ src-tauri/src/
│  ├─ main.rs                                # MeetingFeature, meeting level poll
│  ├─ commands.rs                            # meeting CRUD + start/stop
│  └─ events.rs                              # meeting:state, meeting:segment, meeting:level
└─ ui/src/
   ├─ api.ts                                 # meeting typed wrappers + event listeners
   ├─ App.tsx                                 # nav + Meetings page route
   ├─ components/
   │  ├─ TranscriptPanel.tsx                  # live + historical segments
   │  ├─ MeetingsPanel.tsx                    # start/stop, slot binders
   │  └─ MeetingDetail.tsx                    # notes + title view
   └─ pages/
      └─ MeetingsPage.tsx
```

---

### Task 1: `data.db` migration — `meetings`, `meeting_segments`, `meeting_notes`

**Files:**
- Create: `crates/core/migrations/data/0004_meetings.sql`
- Create: `crates/core/src/store/meetings.rs`
- Modify: `crates/core/src/store/mod.rs`, `crates/core/src/lib.rs`

**Interfaces:**
- Produces tables per spec §6.3 (simplified vs VoxNative — no audio asset blobs in Phase 3):

```sql
CREATE TABLE meetings (
    id              TEXT PRIMARY KEY NOT NULL,
    title           TEXT NOT NULL,
    started_at      TEXT NOT NULL,
    ended_at        TEXT,
    status          TEXT NOT NULL,          -- recording | completed | error
    capture_mode    TEXT NOT NULL,          -- mic_only | mic_and_system
    stt_engine_id   TEXT,
    llm_engine_id   TEXT,
    error           TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE meeting_segments (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id      TEXT NOT NULL REFERENCES meetings(id) ON DELETE CASCADE,
    sequence        INTEGER NOT NULL,
    start_offset_ms INTEGER NOT NULL,
    end_offset_ms   INTEGER NOT NULL,
    text            TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE (meeting_id, sequence)
);

CREATE TABLE meeting_notes (
    meeting_id      TEXT PRIMARY KEY REFERENCES meetings(id) ON DELETE CASCADE,
    summary         TEXT NOT NULL DEFAULT '',
    decisions       TEXT NOT NULL DEFAULT '',
    action_items    TEXT NOT NULL DEFAULT '',
    follow_ups      TEXT NOT NULL DEFAULT '',
    open_questions  TEXT NOT NULL DEFAULT '',
    prompt_version  TEXT NOT NULL,
    engine_id       TEXT,
    model           TEXT,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_meetings_started ON meetings (started_at DESC);
CREATE INDEX idx_meeting_segments_meeting ON meeting_segments (meeting_id, sequence);
```

- [ ] **Step 1: Write the failing test**

`crates/core/src/store/meetings.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_data_migrations};

    #[tokio::test]
    async fn meetings_tables_exist_after_migration() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meetings")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }
}
```

- [ ] **Step 2: Run test — FAIL** (`no such table: meetings`)

Run: `cargo test -p kea-core meetings_tables_exist`

- [ ] **Step 3: Add migration + wire `pub mod meetings`**

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(core): data.db migration for meetings tables"
```

---

### Task 2: `MeetingRepo` — CRUD + segment/note append

**Files:**
- Modify: `crates/core/src/store/meetings.rs`

**Interfaces:**
- Produces:

```rust
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

pub struct NewMeeting {
    pub id: String,
    pub title: String,
    pub capture_mode: String,
    pub stt_engine_id: Option<String>,
    pub llm_engine_id: Option<String>,
}

pub struct NewSegment {
    pub sequence: i32,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
    pub text: String,
}

pub struct MeetingRepo { pool: SqlitePool }

impl MeetingRepo {
    pub fn new(pool: SqlitePool) -> Self;
    pub async fn create(&self, m: &NewMeeting) -> Result<(), KeaError>;
    pub async fn list(&self, limit: i64) -> Result<Vec<Meeting>, KeaError>;
    pub async fn get(&self, id: &str) -> Result<Option<MeetingDetail>, KeaError>;
    pub async fn append_segment(&self, meeting_id: &str, seg: &NewSegment) -> Result<i64, KeaError>;
    pub async fn upsert_notes(&self, notes: &MeetingNotes) -> Result<(), KeaError>;
    pub async fn set_title(&self, id: &str, title: &str) -> Result<(), KeaError>;
    pub async fn complete(&self, id: &str, status: &str, error: Option<&str>) -> Result<(), KeaError>;
    pub async fn delete(&self, id: &str) -> Result<(), KeaError>;
}
```

- [ ] **Step 1: Write the failing test**

```rust
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
    }).await.unwrap();

    let seg_id = repo.append_segment("m1", &NewSegment {
        sequence: 0,
        start_offset_ms: 0,
        end_offset_ms: 30_000,
        text: "Hello everyone".into(),
    }).await.unwrap();
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
    }).await.unwrap();

    repo.set_title("m1", "Weekly Sync").await.unwrap();
    repo.complete("m1", "completed", None).await.unwrap();

    let detail = repo.get("m1").await.unwrap().unwrap();
    assert_eq!(detail.meeting.title, "Weekly Sync");
    assert_eq!(detail.segments.len(), 1);
    assert_eq!(detail.segments[0].text, "Hello everyone");
    assert_eq!(detail.notes.as_ref().unwrap().summary, "Kickoff");
}
```

- [ ] **Step 2–5:** implement repo, PASS, commit.

```bash
git commit --no-verify -m "feat(core): MeetingRepo CRUD with segments and notes"
```

---

### Task 3: Pure helpers — `mix_frames` + `chunk_pcm_by_duration`

**Files:**
- Modify: `crates/platform/src/audio/util.rs`
- Create: `crates/core/src/meetings/segment.rs`
- Modify: `crates/core/src/meetings/mod.rs`, `crates/core/src/lib.rs`

**Interfaces:**
- Produces in `kea-platform`:

```rust
/// Mix two mono frames to mono (resample shorter to match, average samples).
pub fn mix_frames(mic: &PcmFrame, system: &PcmFrame) -> PcmFrame;
```

- Produces in `kea-core`:

```rust
/// Split PCM into fixed-duration chunks for segmented STT (last chunk may be shorter).
pub fn chunk_pcm_by_duration(frame: &PcmFrame, chunk_secs: u32) -> Vec<PcmFrame>;
```

- [ ] **Step 1: Write the failing tests**

`crates/platform/src/audio/util.rs`:

```rust
#[test]
fn mix_frames_averages_aligned_samples() {
    let mic = PcmFrame { samples: vec![1.0, 0.0], sample_rate_hz: 16_000 };
    let sys = PcmFrame { samples: vec![0.0, 1.0], sample_rate_hz: 16_000 };
    let mixed = mix_frames(&mic, &sys);
    assert_eq!(mixed.samples.len(), 2);
    assert!((mixed.samples[0] - 0.5).abs() < 0.01);
    assert!((mixed.samples[1] - 0.5).abs() < 0.01);
}
```

`crates/core/src/meetings/segment.rs`:

```rust
#[test]
fn chunk_90s_audio_into_three_30s_segments() {
    let frame = PcmFrame {
        samples: vec![0.0; 16_000 * 90],
        sample_rate_hz: 16_000,
    };
    let chunks = chunk_pcm_by_duration(&frame, 30);
    assert_eq!(chunks.len(), 3);
    assert_eq!(chunks[0].samples.len(), 16_000 * 30);
    assert_eq!(chunks[2].samples.len(), 16_000 * 30);
}
```

Import `PcmFrame` in core via `kea_platform::PcmFrame` (add `kea-platform` to `kea-core` dev-dependency for tests only, or duplicate a minimal sample vec helper in core — prefer `dev-dependency`).

- [ ] **Step 2–5:** implement, PASS, commit.

```bash
git commit --no-verify -m "feat: mix_frames and chunk_pcm_by_duration helpers"
```

---

### Task 4: `config.db` migration + `MeetingSettings` repo

**Files:**
- Create: `crates/core/migrations/config/0004_meetings.sql`
- Create: `crates/core/src/meetings/settings.rs`

**Interfaces:**
- Canonical settings keys (KV in existing `settings` table):
  - `meetings.segment_duration_secs` → `"30"` (default)
  - `meetings.prefer_system_audio` → `"true"` | `"false"`
- Produces:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeetingSettings {
    pub segment_duration_secs: u32,
    pub prefer_system_audio: bool,
}

pub struct MeetingSettingsRepo { settings: SettingsRepo }
```

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn meeting_settings_roundtrip() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    run_config_migrations(&pool).await.unwrap();
    let repo = MeetingSettingsRepo::new(SettingsRepo::new(pool));
    let cfg = MeetingSettings { segment_duration_secs: 45, prefer_system_audio: true };
    repo.set(&cfg).await.unwrap();
    assert_eq!(repo.get().await.unwrap(), cfg);
}
```

- [ ] **Step 2–5:** implement, PASS, commit.

```bash
git commit --no-verify -m "feat(core): MeetingSettings repo on config.db"
```

---

### Task 5: Meeting synthesis prompts (`build_meeting_notes_prompt`, `build_meeting_title_prompt`)

**Files:**
- Create: `crates/core/src/meetings/synthesis.rs`

**Interfaces:**
- Copy prompt text from `VoxNative/Meetings/MeetingSynthesis.swift` (`meeting-notes-v1`, `meeting-title-v1`).
- Produces:

```rust
pub const MEETING_NOTES_PROMPT_VERSION: &str = "meeting-notes-v1";
pub const MEETING_TITLE_PROMPT_VERSION: &str = "meeting-title-v1";

pub fn format_transcript_for_synthesis(segments: &[MeetingSegment]) -> String;
pub fn build_meeting_notes_request(title: &str, started_at: &str, transcript: &str) -> LlmRequest;
pub fn build_meeting_title_request(summary: &str) -> LlmRequest;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ParsedMeetingNotes {
    pub summary: String,
    pub decisions: String,
    pub action_items: String,
    pub follow_ups: String,
    pub open_questions: String,
}

pub fn parse_meeting_notes_json(content: &str) -> Result<ParsedMeetingNotes, KeaError>;
```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn notes_prompt_wraps_transcript_in_tags() {
    let req = build_meeting_notes_request(
        "Weekly Sync",
        "2026-06-26T10:00:00Z",
        "Alice: hello",
    );
    assert!(req.prompt.contains("<transcript>"));
    assert!(req.prompt.contains("Alice: hello"));
    assert!(req.prompt.contains("summary"));
}

#[test]
fn parses_notes_json_with_snake_case_keys() {
    let json = r#"{"summary":"s","decisions":"d","action_items":"a","follow_ups":"f","open_questions":"q"}"#;
    let parsed = parse_meeting_notes_json(json).unwrap();
    assert_eq!(parsed.action_items, "a");
}
```

- [ ] **Step 2–5:** implement (strip markdown fences like Swift), PASS, commit.

```bash
git commit --no-verify -m "feat(core): meeting synthesis prompts and JSON parser"
```

---

### Task 6: Extend `AudioIo` trait — meeting capture + `SystemAudioCapability`

**Files:**
- Modify: `crates/platform/src/audio/mod.rs`

**Interfaces:**
- Produces (extends existing mic methods — dictation unchanged):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MeetingState {
    Idle,
    Recording,
    Processing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SystemAudioCapability {
    Unavailable,
    ScreenCaptureKit,   // macOS 13+, gated impl
    LoopbackDevice,     // cpal virtual device present
    MicOnly,            // explicit fallback
}

#[async_trait]
pub trait AudioIo: Send + Sync {
    // --- existing dictation ---
    async fn start_mic(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError>;
    async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError>;
    fn current_level(&self) -> f32;
    fn state(&self) -> DictationState;

    // --- Phase 3 meetings ---
    fn system_audio_capability(&self) -> SystemAudioCapability;
    fn meeting_state(&self) -> MeetingState;

    /// Begin meeting capture (mic + system when available). Frames on `frame_rx`.
    async fn start_meeting(
        &mut self,
    ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError>;

    /// Stop meeting capture; return full mixed mono PCM buffer.
    async fn stop_meeting(&mut self) -> Result<PcmFrame, AudioIoError>;

    /// Drain frames accumulated since last drain (for live segmented transcription).
    async fn drain_meeting_buffer(&mut self) -> Result<PcmFrame, AudioIoError>;
}
```

- [ ] **Step 1: Extend `FakeAudioIo` test in `audio/mod.rs`**

```rust
struct FakeMeetingAudioIo {
    dictation_state: DictationState,
    meeting_state: MeetingState,
    capability: SystemAudioCapability,
    buffered: PcmFrame,
    pending_drains: Vec<PcmFrame>,
}

#[async_trait]
impl AudioIo for FakeMeetingAudioIo {
    // ... existing mic methods delegate ...
    fn system_audio_capability(&self) -> SystemAudioCapability { self.capability }
    fn meeting_state(&self) -> MeetingState { self.meeting_state }
    async fn start_meeting(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
        self.meeting_state = MeetingState::Recording;
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        Ok(rx)
    }
    async fn stop_meeting(&mut self) -> Result<PcmFrame, AudioIoError> {
        self.meeting_state = MeetingState::Idle;
        Ok(self.buffered.clone())
    }
    async fn drain_meeting_buffer(&mut self) -> Result<PcmFrame, AudioIoError> {
        Ok(self.pending_drains.pop().unwrap_or(PcmFrame { samples: vec![], sample_rate_hz: 16_000 }))
    }
}

#[tokio::test]
async fn fake_meeting_audio_drains_segments() {
    let mut io = FakeMeetingAudioIo { /* capability: MicOnly, pending_drains: vec![one frame] */ };
    let _rx = io.start_meeting().await.unwrap();
    assert_eq!(io.meeting_state(), MeetingState::Recording);
    let chunk = io.drain_meeting_buffer().await.unwrap();
    assert_eq!(chunk.samples.len(), 1600);
}
```

- [ ] **Step 2: Run test — FAIL** (trait methods missing on `StubAudioIo`)

- [ ] **Step 3: Add trait methods; implement on `stub.rs` and `macos.rs` as `todo!()` or mic-only stubs returning errors until Task 8**

Default stub:

```rust
async fn start_meeting(&mut self) -> Result<_, _> {
    Err(AudioIoError::Other("meeting capture not implemented".into()))
}
fn system_audio_capability(&self) -> SystemAudioCapability { SystemAudioCapability::Unavailable }
fn meeting_state(&self) -> MeetingState { MeetingState::Idle }
async fn drain_meeting_buffer(&mut self) -> Result<PcmFrame, AudioIoError> {
    Ok(PcmFrame { samples: vec![], sample_rate_hz: 16_000 })
}
async fn stop_meeting(&mut self) -> Result<PcmFrame, AudioIoError> {
    Err(AudioIoError::Other("meeting capture not implemented".into()))
}
```

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(platform): AudioIo meeting capture trait extension"
```

---

### Task 7: Platform `Permissions` trait + macOS Screen Recording

**Files:**
- Create: `crates/platform/src/permissions/mod.rs`, `macos.rs`, `stub.rs`
- Modify: `crates/platform/src/lib.rs`

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermKind {
    Microphone,
    ScreenRecording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermStatus {
    Unknown,
    Granted,
    Denied,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PermError { #[error("{0}")] Other(String) }

pub trait Permissions: Send + Sync {
    fn status(&self, kind: PermKind) -> PermStatus;
    async fn request(&self, kind: PermKind) -> Result<PermStatus, PermError>;
}

pub fn new_permissions() -> Box<dyn Permissions>;
```

macOS `ScreenRecording` uses `CGPreflightScreenCaptureAccess` / `CGRequestScreenCaptureAccess` via `core-graphics` or `objc2` — **unit test only parses status enum**; no real permission dialog in CI.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn perm_status_serializes() {
    let json = serde_json::to_string(&PermStatus::Granted).unwrap();
    assert_eq!(json, r#""Granted""#);
}
```

- [ ] **Step 2–5:** implement macOS + stub, export `new_permissions()`, commit.

```bash
git commit --no-verify -m "feat(platform): Permissions trait with macOS Screen Recording"
```

---

### Task 8: macOS `AudioIo` — mic-only meeting capture (baseline)

**Files:**
- Modify: `crates/platform/src/audio/macos.rs`

**Interfaces:**
- `start_meeting` opens the same mic stream as `start_mic` but sets `meeting_state = Recording` and accumulates frames in a `meeting_buffer: Vec<PcmFrame>`.
- `drain_meeting_buffer` returns accumulated frames since last drain (then clears).
- `stop_meeting` stops stream, returns `accumulate_frames(&meeting_buffer)`.
- `system_audio_capability()` returns `MicOnly` until Tasks 9–10 enhance.

- [ ] **Step 1: Write the failing test (state machine only)**

```rust
#[test]
fn meeting_state_starts_idle() {
    let io = MacAudioIo::new_for_test();
    assert_eq!(io.meeting_state(), MeetingState::Idle);
    assert_eq!(io.system_audio_capability(), SystemAudioCapability::MicOnly);
}
```

- [ ] **Step 2–5:** implement meeting buffer alongside existing dictation stream (mutual exclusion: cannot dictation+meeting simultaneously — return error if wrong state), PASS, commit.

```bash
git commit --no-verify -m "feat(platform): macOS mic-only meeting capture"
```

---

### Task 9: macOS loopback device detection (cpal / BlackHole best-effort)

**Files:**
- Create: `crates/platform/src/audio/loopback.rs`
- Modify: `crates/platform/src/audio/macos.rs`

**Interfaces:**
- Produces:

```rust
pub fn find_loopback_input_device(host: &cpal::Host) -> Option<cpal::Device>;
pub fn device_display_name(device: &cpal::Device) -> Option<String>;
```

Detection heuristic: input device name contains `"BlackHole"`, `"Loopback"`, or `"Monitor"`.

When found, `MacAudioIo::system_audio_capability()` returns `LoopbackDevice`; `start_meeting` captures mic + loopback device streams, mixes per-frame with `mix_frames`.

- [ ] **Step 1: Write the failing test (pure name matcher)**

```rust
#[test]
fn recognizes_blackhole_as_loopback() {
    assert!(is_loopback_device_name("BlackHole 2ch"));
    assert!(!is_loopback_device_name("MacBook Pro Microphone"));
}
```

- [ ] **Step 2–5:** implement detection + mixed capture path (no real device in test), commit.

```bash
git commit --no-verify -m "feat(platform): macOS cpal loopback device detection and mix"
```

---

### Task 10: **[GATED]** macOS ScreenCaptureKit system audio (`system-audio-sck` feature)

**Files:**
- Create: `crates/platform/src/audio/macos_sck.rs`
- Modify: `crates/platform/Cargo.toml`, root `Cargo.toml`, `crates/platform/src/audio/macos.rs`

**Interfaces:**
- `kea-platform/Cargo.toml`:

```toml
[features]
default = []
system-audio-sck = ["dep:screencapturekit"]

[dependencies.screencapturekit]
version = "7"
optional = true
features = ["macos_13_0", "async"]
```

- Produces `SckSystemAudioCapture` implementing a small trait (testable seam):

```rust
#[async_trait]
pub trait SystemAudioCapture: Send + Sync {
    async fn start(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError>;
    async fn stop(&mut self) -> Result<PcmFrame, AudioIoError>;
}
```

`MacAudioIo` when `cfg(feature = "system-audio-sck")` and Screen Recording granted: capability = `ScreenCaptureKit`; mixes SCK frames with mic.

- [ ] **Step 1: Write the failing test with `FakeSystemAudioCapture`**

```rust
struct FakeSck;
#[async_trait]
impl SystemAudioCapture for FakeSck {
    async fn start(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
        let (tx, rx) = tokio::sync::mpsc::channel(4);
        tx.send(PcmFrame { samples: vec![0.5; 100], sample_rate_hz: 48_000 }).await.ok();
        Ok(rx)
    }
    async fn stop(&mut self) -> Result<PcmFrame, AudioIoError> {
        Ok(PcmFrame { samples: vec![0.5; 100], sample_rate_hz: 48_000 })
    }
}

#[test]
fn sck_fake_produces_frames() {
    // compile-only seam test without feature
}
```

- [ ] **Step 2: Spike (manual, not CI):** `cargo build -p kea-platform --features system-audio-sck` links `screencapturekit`; run example capture 5s with Screen Recording granted.

- [ ] **Step 3–5:** implement `SckSystemAudioCapture` behind feature; wire into `MacAudioIo`; document failure modes in code comment; commit.

```bash
git commit --no-verify -m "feat(platform): ScreenCaptureKit system audio behind system-audio-sck feature"
```

> **If spike fails:** stop; leave `system-audio-sck` feature stub returning `Unavailable`; document blocker in plan PR. Phase 3 still ships mic-only + BlackHole.

---

### Task 11: `MeetingFeature` declaration

**Files:**
- Create: `crates/features/src/meetings.rs`
- Modify: `crates/features/src/lib.rs`

**Interfaces:**

```rust
pub struct MeetingFeature;

impl Feature for MeetingFeature {
    fn id(&self) -> &str { "meetings" }
    fn required_caps(&self) -> Vec<CapSlot> {
        vec![
            CapSlot { name: "stt", kind: CapKind::Stt },
            CapSlot { name: "llm", kind: CapKind::Llm },
        ]
    }
    fn commands(&self) -> Vec<Command> {
        vec![Command {
            id: "toggle_meeting".into(),
            title: "Start / Stop Meeting".into(),
            default_accelerator: Some(default_meeting_accelerator().into()),
        }]
    }
}
```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn meetings_declares_stt_and_llm_slots() {
    let f = MeetingFeature;
    assert_eq!(f.id(), "meetings");
    assert_eq!(f.required_caps().len(), 2);
    assert_eq!(f.required_caps()[0].name, "stt");
    assert_eq!(f.required_caps()[1].name, "llm");
}
```

- [ ] **Step 2–5:** implement, export, commit.

```bash
git commit --no-verify -m "feat(features): MeetingFeature with stt and llm slots"
```

---

### Task 12: `transcribe_meeting_segment()` — single segment STT call

**Files:**
- Modify: `crates/features/src/meetings.rs`

**Interfaces:**
- Produces:

```rust
pub async fn transcribe_meeting_segment(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    audio: &PcmFrame,
    settings: &MeetingSettings,
) -> Result<String, String>;
```

Resolves `meetings`/`stt` slot, converts `PcmFrame` → `AudioPcm` (16 kHz), calls `SttEngine::transcribe`.

- [ ] **Step 1: Write the failing test**

```rust
struct FakeStt { text: String }
// ... impl SttEngine returning text ...

#[tokio::test]
async fn transcribe_segment_uses_meetings_stt_binding() {
    let mut reg = EngineRegistry::default();
    reg.register_stt(Arc::new(FakeStt { text: "segment text".into() }));
    let (bindings, _, _, _) = test_repos().await;
    bindings.set("meetings", "stt", Binding {
        engine_id: "fake-stt".into(), model: None, provider_ref: None,
    }).await.unwrap();
    let out = transcribe_meeting_segment(
        &reg, &bindings,
        &PcmFrame { samples: vec![0.0; 1600], sample_rate_hz: 16_000 },
        &MeetingSettings::default(),
    ).await.unwrap();
    assert_eq!(out, "segment text");
}
```

- [ ] **Step 2–5:** implement, PASS, commit.

```bash
git commit --no-verify -m "feat(features): transcribe_meeting_segment helper"
```

---

### Task 13: `synthesize_meeting_notes()` + `synthesize_meeting_title()`

**Files:**
- Modify: `crates/features/src/meetings.rs`

**Interfaces:**

```rust
pub async fn synthesize_meeting_notes(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meeting: &Meeting,
    segments: &[MeetingSegment],
) -> Result<MeetingNotes, String>;

pub async fn synthesize_meeting_title(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    summary: &str,
) -> Result<String, String>;
```

Uses `build_meeting_notes_request` / `build_meeting_title_request` + `LlmEngine::complete` + `parse_meeting_notes_json`.

- [ ] **Step 1: Write the failing test with `FakeLlm` returning JSON**

```rust
struct FakeLlm;
#[async_trait]
impl LlmEngine for FakeLlm {
    fn id(&self) -> &str { "fake-llm" }
    fn capabilities(&self) -> EngineCaps { EngineCaps { models: vec![] } }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
        if req.prompt.contains("title") || req.prompt.contains("Title") {
            return Ok(LlmResponse { text: "Sprint Planning".into() });
        }
        Ok(LlmResponse { text: r#"{"summary":"s","decisions":"","action_items":"","follow_ups":"","open_questions":""}"#.into() })
    }
}

#[tokio::test]
async fn synthesize_notes_parses_llm_json() {
    // register FakeLlm, bind meetings/llm, assert MeetingNotes.summary == "s"
}
```

- [ ] **Step 2–5:** implement, PASS, commit.

```bash
git commit --no-verify -m "feat(features): meeting notes and title LLM synthesis"
```

---

### Task 14: `run_meeting()` orchestration (full flow)

**Files:**
- Modify: `crates/features/src/meetings.rs`
- Modify: `crates/features/Cargo.toml` (dep `kea-core` meetings types)

**Interfaces:**

```rust
pub struct MeetingRunContext<'a> {
    pub engines: &'a EngineRegistry,
    pub bindings: &'a BindingRepo,
    pub actions: &'a ActionRepo,
    pub meetings: &'a MeetingRepo,
    pub audio: &'a mut dyn AudioIo,
    pub settings: &'a MeetingSettings,
}

pub struct MeetingSegmentEvent {
    pub meeting_id: String,
    pub sequence: i32,
    pub text: String,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
}

pub async fn run_meeting_start(ctx: &MeetingRunContext<'_>) -> Result<String, String>;
pub async fn run_meeting_poll_segment(
    ctx: &MeetingRunContext<'_>,
    meeting_id: &str,
    sequence: &mut i32,
    elapsed_ms: &mut i64,
) -> Result<Option<MeetingSegmentEvent>, String>;
pub async fn run_meeting_stop(ctx: &MeetingRunContext<'_>, meeting_id: &str) -> Result<MeetingDetail, String>;
```

**`run_meeting_start` flow:**
1. Resolve STT + LLM engine ids (for metadata).
2. Determine `capture_mode` from `audio.system_audio_capability()` (`mic_and_system` vs `mic_only`).
3. `meetings.create(NewMeeting { ... })`.
4. `actions.record(NewAction { feature_id: "meetings", command: "toggle_meeting", ... })`.
5. `audio.start_meeting()`.

**`run_meeting_poll_segment` flow (called by Tauri timer every `segment_duration_secs`):**
1. `pcm = audio.drain_meeting_buffer()`.
2. Skip if fewer than 1s of samples.
3. `text = transcribe_meeting_segment(...)`.
4. `meetings.append_segment(...)`.
5. Return `MeetingSegmentEvent` for Tauri emit.

**`run_meeting_stop` flow:**
1. Final drain + transcribe if non-empty.
2. `audio.stop_meeting()`.
3. Load segments; `synthesize_meeting_notes`; `meetings.upsert_notes`.
4. `title = synthesize_meeting_title(notes.summary)`; `meetings.set_title`.
5. `meetings.complete(..., "completed", None)`; `actions.finish`.

- [ ] **Step 1: Write the failing integration test**

```rust
#[tokio::test]
async fn run_meeting_persists_segments_and_notes() {
    let mut reg = EngineRegistry::default();
    reg.register_stt(Arc::new(FakeStt { text: "hello".into() }));
    reg.register_llm(Arc::new(FakeLlm));

    let config_pool = open_pool("sqlite::memory:").await.unwrap();
    run_config_migrations(&config_pool).await.unwrap();
    let data_pool = open_pool("sqlite::memory:").await.unwrap();
    run_data_migrations(&data_pool).await.unwrap();

    let bindings = BindingRepo::new(config_pool);
    let actions = ActionRepo::new(data_pool.clone());
    let meetings = MeetingRepo::new(data_pool);

    let mut audio = FakeMeetingAudioIo {
        capability: SystemAudioCapability::MicOnly,
        pending_drains: vec![
            PcmFrame { samples: vec![0.0; 16_000], sample_rate_hz: 16_000 },
        ],
        buffered: PcmFrame { samples: vec![0.0; 8000], sample_rate_hz: 16_000 },
        ..Default::default()
    };

    let settings = MeetingSettings { segment_duration_secs: 30, prefer_system_audio: false };
    let ctx = MeetingRunContext { engines: &reg, bindings: &bindings, actions: &actions, meetings: &meetings, audio: &mut audio, settings: &settings };

    let meeting_id = run_meeting_start(&ctx).await.unwrap();
    let mut seq = 0;
    let mut elapsed = 0;
    let ev = run_meeting_poll_segment(&ctx, &meeting_id, &mut seq, &mut elapsed).await.unwrap();
    assert!(ev.is_some());
    assert_eq!(ev.unwrap().text, "hello");

    let detail = run_meeting_stop(&ctx, &meeting_id).await.unwrap();
    assert!(!detail.meeting.title.is_empty() || detail.meeting.title == "Untitled Meeting");
    assert!(!detail.segments.is_empty());
    assert!(detail.notes.is_some());
}
```

- [ ] **Step 2–5:** implement, PASS, commit.

```bash
git commit --no-verify -m "feat(features): run_meeting orchestration with segmented STT and LLM synthesis"
```

---

### Task 15: Tauri `AppState` — register `MeetingFeature` + `MeetingRepo`

**Files:**
- Modify: `src-tauri/src/main.rs`, `src-tauri/Cargo.toml`

**Interfaces:**
- Extend `AppState`:

```rust
pub struct AppState {
    // existing...
    pub permissions: Box<dyn kea_platform::Permissions>,
    pub active_meeting: Mutex<Option<String>>,  // meeting id while recording
}
```

- `features.register(Arc::new(MeetingFeature))`.
- Store `permissions: new_permissions()` in setup.

- [ ] **Step 1: Write failing test** in `commands.rs`:

```rust
#[test]
fn meeting_feature_is_registered() {
    let reg = FeatureRegistry::default();
    // after setup helper registers MeetingFeature
    assert!(reg.list_ids().contains(&"meetings".to_string()));
}
```

- [ ] **Step 2–5:** wire, commit.

```bash
git commit --no-verify -m "feat(app): AppState with MeetingFeature and permissions"
```

---

### Task 16: Tauri commands — meetings CRUD + start/stop/poll + permissions

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/main.rs`

**Interfaces:**

```rust
pub const MEETINGS_FEATURE_ID: &str = "meetings";
pub const MEETINGS_COMMAND_ID: &str = "toggle_meeting";

#[tauri::command] async fn get_meeting_settings(state) -> MeetingSettings
#[tauri::command] async fn set_meeting_settings(state, settings: MeetingSettings) -> Result<(), String>
#[tauri::command] async fn get_system_audio_capability(state) -> String  // serde of SystemAudioCapability
#[tauri::command] async fn get_permission_status(state, kind: String) -> PermStatus
#[tauri::command] async fn request_permission(state, kind: String) -> Result<PermStatus, String>
#[tauri::command] async fn list_meetings(state, limit: Option<i64>) -> Vec<Meeting>
#[tauri::command] async fn get_meeting(state, id: String) -> Result<MeetingDetail, String>
#[tauri::command] async fn delete_meeting(state, id: String) -> Result<(), String>
#[tauri::command] async fn start_meeting(state, app: AppHandle) -> Result<String, String>
#[tauri::command] async fn stop_meeting(state, app: AppHandle) -> Result<MeetingDetail, String>
```

- `start_meeting`: calls `run_meeting_start`, stores id in `active_meeting`, spawns segment poll task (interval = `settings.segment_duration_secs`), spawns level poll (reuse dictation level pattern), emits `meeting:state` = `"recording"`.
- `stop_meeting`: cancels poll tasks, calls `run_meeting_stop`, clears `active_meeting`, emits `meeting:state` = `"idle"`.

- [ ] **Step 1: Write failing test** for `system_audio_capability_dto` pure helper.

- [ ] **Step 2–5:** implement commands, register in `generate_handler!`, commit.

```bash
git commit --no-verify -m "feat(app): Tauri commands for meetings and permissions"
```

---

### Task 17: Tauri events — `meeting:state`, `meeting:segment`, `meeting:level`

**Files:**
- Modify: `src-tauri/src/events.rs`

**Interfaces:**

```rust
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MeetingStatePayload { pub state: String }  // idle | recording | processing

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MeetingSegmentPayload {
    pub meeting_id: String,
    pub sequence: i32,
    pub text: String,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct MeetingLevelPayload { pub level: f32 }

pub fn emit_meeting_state(app: &AppHandle, state: &str);
pub fn emit_meeting_segment(app: &AppHandle, seg: &MeetingSegmentPayload);
pub fn emit_meeting_level(app: &AppHandle, level: f32);
pub fn emit_meeting_error(app: &AppHandle, message: &str);
```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn meeting_segment_payload_serializes() {
    let json = serde_json::to_string(&MeetingSegmentPayload {
        meeting_id: "m1".into(), sequence: 0, text: "hi".into(),
        start_offset_ms: 0, end_offset_ms: 30000,
    }).unwrap();
    assert!(json.contains(r#""text":"hi""#));
}
```

- [ ] **Step 2–5:** implement emitters; wire poll task to `emit_meeting_segment`, commit.

```bash
git commit --no-verify -m "feat(app): meeting Tauri events for state, segments, and level"
```

---

### Task 18: UI `api.ts` — meetings API + event listeners

**Files:**
- Modify: `ui/src/api.ts`

**Interfaces:**

```ts
export type MeetingState = "idle" | "recording" | "processing";
export type SystemAudioCapability =
  | "unavailable"
  | "screen_capture_kit"
  | "loopback_device"
  | "mic_only";

export type Meeting = {
  id: string;
  title: string;
  started_at: string;
  ended_at: string | null;
  status: string;
  capture_mode: string;
};

export type MeetingSegment = {
  id: number;
  meeting_id: string;
  sequence: number;
  start_offset_ms: number;
  end_offset_ms: number;
  text: string;
};

export type MeetingNotes = {
  summary: string;
  decisions: string;
  action_items: string;
  follow_ups: string;
  open_questions: string;
};

export type MeetingDetail = {
  meeting: Meeting;
  segments: MeetingSegment[];
  notes: MeetingNotes | null;
};

export type MeetingSettings = {
  segment_duration_secs: number;
  prefer_system_audio: boolean;
};

export const getMeetingSettings = () => invoke<MeetingSettings>("get_meeting_settings");
export const setMeetingSettings = (settings: MeetingSettings) =>
  invoke<void>("set_meeting_settings", { settings });
export const getSystemAudioCapability = () =>
  invoke<SystemAudioCapability>("get_system_audio_capability");
export const getPermissionStatus = (kind: "microphone" | "screen_recording") =>
  invoke<string>("get_permission_status", { kind });
export const requestPermission = (kind: "microphone" | "screen_recording") =>
  invoke<string>("request_permission", { kind });
export const listMeetings = (limit?: number) =>
  invoke<Meeting[]>("list_meetings", { limit });
export const getMeeting = (id: string) => invoke<MeetingDetail>("get_meeting", { id });
export const deleteMeeting = (id: string) => invoke<void>("delete_meeting", { id });
export const startMeeting = () => invoke<string>("start_meeting");
export const stopMeeting = () => invoke<MeetingDetail>("stop_meeting");

export const onMeetingState = (cb: (state: MeetingState) => void) =>
  listen<{ state: MeetingState }>("meeting:state", (e) => cb(e.payload.state));
export const onMeetingSegment = (cb: (seg: MeetingSegment) => void) =>
  listen<MeetingSegment>("meeting:segment", (e) => cb(e.payload));
export const onMeetingLevel = (cb: (level: number) => void) =>
  listen<{ level: number }>("meeting:level", (e) => handler(e.payload.level));
export const onMeetingError = (cb: (message: string) => void) =>
  listen<{ message: string }>("meeting:error", (e) => cb(e.payload.message));
```

- [ ] **Step 1:** `npm run typecheck` — FAIL until commands exist.
- [ ] **Step 2–5:** add exports, PASS, commit.

```bash
git commit --no-verify -m "feat(ui): typed Phase 3 meetings API"
```

---

### Task 19: UI `TranscriptPanel` component

**Files:**
- Create: `ui/src/components/TranscriptPanel.tsx`

**Interfaces:**
- Props: `{ segments: MeetingSegment[]; live?: boolean }` — scrollable list, monospace timestamps, auto-scroll when `live`.

- [ ] **Step 1:** typecheck import from `MeetingsPanel.tsx`.
- [ ] **Step 2–5:** implement, commit.

```bash
git commit --no-verify -m "feat(ui): TranscriptPanel component"
```

---

### Task 20: UI `MeetingsPanel` + `MeetingDetail`

**Files:**
- Create: `ui/src/components/MeetingsPanel.tsx`, `ui/src/components/MeetingDetail.tsx`
- Create: `ui/src/pages/MeetingsPage.tsx`
- Modify: `ui/src/App.tsx`

**Interfaces:**
- `MeetingsPanel`: composes `SlotBinder` (`meetings`/`stt`, `meetings`/`llm`), shows `SystemAudioCapability` badge, Screen Recording request button (macOS), Start/Stop buttons, live `TranscriptPanel` subscribed to `onMeetingSegment` + `onMeetingLevel` (reuse `LevelMeter`).
- `MeetingDetail`: renders title, notes sections (summary, decisions, action items, …), full transcript.
- `MeetingsPage`: split view — meeting list (left) + detail (right); calls `listMeetings` / `getMeeting` / `deleteMeeting`.
- `App.tsx`: add **Meetings** nav tab.

```tsx
// MeetingsPanel excerpt
export default function MeetingsPanel() {
  const [state, setState] = useState<MeetingState>("idle");
  const [segments, setSegments] = useState<MeetingSegment[]>([]);
  const [capability, setCapability] = useState<SystemAudioCapability>("mic_only");

  useEffect(() => {
    void getSystemAudioCapability().then(setCapability);
    const unsubs = Promise.all([
      onMeetingState(setState),
      onMeetingSegment((seg) => setSegments((prev) => [...prev, seg])),
    ]);
    return () => { void unsubs.then((fns) => fns.forEach((fn) => fn())); };
  }, []);
  // Start / Stop wired to startMeeting / stopMeeting
}
```

- [ ] **Step 1–5:** implement pages, wire nav, commit.

```bash
git commit --no-verify -m "feat(ui): Meetings page with live transcript and notes view"
```

---

### Task 21: Optional meeting hotkey dispatcher

**Files:**
- Modify: `src-tauri/src/main.rs`, `src-tauri/src/commands.rs`

**Interfaces:**
- Register `meetings:toggle_meeting` hotkey; toggle start/stop (only when not dictating).

- [ ] **Step 1:** register alongside dictation hotkey in setup.
- [ ] **Step 2–5:** dispatcher branch, commit.

```bash
git commit --no-verify -m "feat(app): meeting toggle hotkey dispatcher"
```

---

### Task 22: **[PARALLEL — Windows]** WASAPI loopback capture

**Files:**
- Create: `crates/platform/src/audio/windows_loopback.rs`
- Modify: `crates/platform/src/lib.rs`, `crates/platform/src/audio/macos.rs` (shared mixed-capture logic → `mixed_capture.rs` if needed)

**Interfaces:**
- `WindowsAudioIo::system_audio_capability()` → `LoopbackDevice` when WASAPI loopback available via cpal.
- Unit test: device name heuristic only.

- [ ] **TDD + manual Windows hardware check.**
- [ ] **Commit:**

```bash
git commit --no-verify -m "feat(platform): Windows WASAPI loopback for meetings"
```

---

### Task 23: **[PARALLEL — Linux]** Pulse/PipeWire monitor source

**Files:**
- Create: `crates/platform/src/audio/linux_monitor.rs`

**Interfaces:**
- Enumerate cpal input devices; select name containing `.monitor` or `Monitor of`.
- Document PipeWire/Pulse setup in `docs/cross-platform/plans/CONTRACTS.md` (one paragraph).

- [ ] **TDD + manual Linux check.**
- [ ] **Commit:**

```bash
git commit --no-verify -m "feat(platform): Linux monitor source for meeting loopback"
```

---

### Task 24: End-to-end acceptance (macOS manual + CI compile)

**Files:** none (verification only)

- [ ] **Step 1: CI (default features — no ScreenCaptureKit)**

Run: `cargo test --workspace && cargo build -p kea-app && (cd ui && npm run build)`
Expected: PASS on macOS, Windows, Linux matrix jobs.

- [ ] **Step 2: Optional SCK build (manual / separate CI job)**

Run: `cargo build -p kea-app --features system-audio-sck`
Expected: compiles; **not** required for default CI green.

- [ ] **Step 3: macOS manual checklist (mic-only — required)**

1. `cargo tauri dev`
2. Grant Microphone when prompted.
3. Configuration → OpenAI credentials; Features → bind `meetings` `stt` + `llm` slots.
4. Meetings → Start → speak for 30s → live segments appear via `meeting:segment` events.
5. Stop → title + notes populated; `data.db` `meetings`, `meeting_segments`, `meeting_notes` rows present.
6. `capture_mode` = `mic_only` when loopback unavailable.

- [ ] **Step 4: macOS manual checklist (loopback — best effort)**

1. Install BlackHole OR build with `--features system-audio-sck`.
2. For SCK: grant Screen Recording in System Settings; `get_permission_status("screen_recording")` → Granted.
3. Start meeting → `capture_mode` = `mic_and_system`; verify remote audio appears in transcript (manual).

- [ ] **Step 5: Document deferred platform checks**

Windows Task 22 / Linux Task 23 manual checks when parallel tasks land.

---

## Phase 3 Definition of Done

- `cargo test --workspace` green **without** `--features system-audio-sck`; `cargo build -p kea-app` succeeds; `ui` builds on CI (macOS, Windows, Linux).
- **macOS (required):** mic-only meeting → segmented transcription → LLM notes + title → persisted in `data.db` → Meetings UI shows live transcript and final notes.
- **macOS (best effort):** `--features system-audio-sck` + Screen Recording permission OR BlackHole loopback → `capture_mode = mic_and_system`.
- `data.db` tables `meetings`, `meeting_segments`, `meeting_notes` migrated; `MeetingRepo` unit-tested in-memory.
- `AudioIo` extended with meeting capture; `Permissions` trait exposes Screen Recording status/request on macOS.
- `MeetingFeature` declares `stt` + `llm` slots; `run_meeting_*` orchestration unit-tested with fake audio/STT/LLM.
- Tauri commands: start/stop/list/get/delete meetings, settings, permission probes; events `meeting:state`, `meeting:segment`, `meeting:level`.
- UI: Meetings page with start/stop, live `TranscriptPanel`, past meetings list, notes detail.
- Unit tests use fakes — no real mic, loopback, network, or ScreenCaptureKit in `cargo test`.

## Self-Review (spec coverage map)

| Spec reference | Plan tasks |
|----------------|------------|
| §3 D9 config.db / data.db / keyring boundary | Tasks 1–2, 4, 16 (content in data.db; settings in config.db) |
| §3 D14 meeting audio mic + system/loopback | Tasks 6, 8–10, 22–23; Global Constraints decision table |
| §4.2 `AudioIo::capture_system` / mixed capture | Tasks 6, 8–10 (via `start_meeting` mixed path) |
| §4.2 `Permissions` Screen Recording | Tasks 7, 16 |
| §4.3 Meetings feature plugin | Tasks 11, 14 |
| §4.4 slot resolution (`stt`, `llm`) | Tasks 12–14 (`resolve_stt`/`resolve_llm` for `"meetings"`) |
| §5 Meetings data flow (5 steps) | Tasks 8–14, 16–17 |
| §6.3 `meetings` + `meeting_segments` + `meeting_notes` | Tasks 1–2 |
| §7 `TranscriptPanel`, overlay primitives | Tasks 19–20 |
| §8 integration matrix System/loopback row | Tasks 9–10, 22–23 |
| §9 Phase 3 outcome | Definition of Done |
| §11 testing strategy (mocked engines, store tests) | Global Constraints; Tasks 2, 14 |
| §12 Risks system-audio capture | Global Constraints; mic-only fallback Tasks 8, 14, 24 |

### How tests avoid real I/O

| Risk | Mitigation |
|------|------------|
| Real microphone / loopback | `FakeMeetingAudioIo` in Tasks 6, 14; macOS impl tests cover state machine only |
| ScreenCaptureKit native code | `system-audio-sck` feature off in CI; `FakeSystemAudioCapture` seam in Task 10 |
| Real OpenAI / LLM network | `FakeLlm` / `NoopLlmEngine` in Tasks 13–14 |
| Real STT network | `FakeStt` in Tasks 12–14 |
| SQLite persistence | In-memory `sqlite::memory:` in Tasks 1–2, 14 |

### Deferred to later phases (explicit boundaries)

| Item | Phase | Notes |
|------|-------|-------|
| **Streaming partial transcription** | Non-goal (§2) | Segmented batch `transcribe` per chunk |
| **Speaker diarization** | Phase 4+ | Segments have no `speaker_label` in Phase 3 schema |
| **Meeting audio file retention** | Phase 4+ | No `meeting_audio_assets` table in Phase 3 |
| **Parakeet STT (D6)** | Phase 4 | Meetings use bound STT slot (OpenAI / Whisper) |
| **History page for meetings** | Phase 4 | Meetings page is the Phase 3 surface; global History deferred |
| **Windows/Linux loopback E2E** | Parallel Tasks 22–23 | May trail macOS; mic-only stubs compile |
| **Full D14 on all OSes** | Phase 3 parallel + Phase 4 polish | Phase 3 DoD = macOS mic-only + best-effort loopback |

### Phase 3 system-audio decision summary

- **Chosen primary loopback technology (macOS):** `screencapturekit` crate behind `system-audio-sck` cargo feature.
- **Chosen opportunistic path:** cpal virtual loopback device (BlackHole).
- **Guaranteed fallback:** mic-only — `MeetingFeature` fully functional without loopback.
- **Explicitly not claimed in default build:** system audio capture "done" without feature flag + manual acceptance.
