# Cross-Phase Contracts (canonical reference for all phase plans)

This file pins the interfaces, module paths, type names, and conventions shared
across phase plans so each plan stays consistent. **Phase plans MUST use these
exact names and signatures.** Within-phase-only types are defined inside each
plan. Reference spec: `../2026-05-29-cross-platform-rewrite-design.md`.

## Workspace layout (extends Phase 0)

Phase 0 created the flat workspace with `crates/core` (modules `settings`, `secrets`) and
`src-tauri` + `ui`. Later phases ADD:

```
crates/core/src/
  rewrite.rs        (Phase 1)   provider trait, OpenAI provider, presets, prompt catalog, service
  speech.rs         (Phase 2/4) Transcriber trait, RemoteTranscriber, TTS trait + OpenAiTts, engine factory
crates/platform/                 (Phase 1, new workspace member)
  Cargo.toml
  src/lib.rs        re-exports: hotkeys, textio, audio (audio added Phase 2), system
  src/hotkeys.rs    (Phase 1)
  src/textio.rs     (Phase 1)
  src/audio.rs      (Phase 2 capture; Phase 4 playback)
  src/system.rs     (Phase 5: autostart, notifications, permissions)
crates/infer/                    (Phase 3, new workspace member)
  Cargo.toml
  src/lib.rs        re-exports: model_manager, local_transcriber
  src/model_manager.rs
  src/local_transcriber.rs
```

When a phase adds a workspace member, it edits the root `Cargo.toml` `members`.
Crate package names: `kea-core`, `kea-platform`, `kea-infer`, binary `kea`.

## core::rewrite (Phase 1)

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RewriteRequest { pub text: String, pub prompt: String, pub model: String }

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RewriteResult { pub text: String }

#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    #[error("http {0}: {1}")] Http(u16, String),
    #[error("network: {0}")] Network(String),
    #[error("empty input")] EmptyInput,
    #[error("invalid configuration: {0}")] Config(String),
}

#[async_trait::async_trait]
pub trait RewriteProvider: Send + Sync {
    async fn rewrite(&self, request: RewriteRequest) -> Result<RewriteResult, RewriteError>;
}

pub struct OpenAiRewriteProvider { /* api_key: String, base_url: String, http: reqwest::Client */ }
impl OpenAiRewriteProvider { pub fn new(api_key: String, base_url: String) -> Self }
// impls RewriteProvider; base_url default "https://api.openai.com/v1"; OpenAI-compatible = custom base_url.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Preset { pub id: String, pub name: String, pub prompt: String, pub model: String }

pub struct PromptCatalog;                       // built-in default presets
impl PromptCatalog { pub fn defaults() -> Vec<Preset> }
```

## platform::hotkeys (Phase 1)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyId { Rewrite, Speech }

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError { #[error("{0}")] Register(String) }

/// Registers global accelerators (e.g. "CmdOrCtrl+Shift+R") and delivers press
/// events on a channel. On Linux, an X11 backend and a Wayland
/// (org.freedesktop.portal.GlobalShortcuts) backend are selected at runtime.
pub trait HotkeyManager: Send {
    fn register(&mut self, id: HotkeyId, accelerator: &str) -> Result<(), HotkeyError>;
    fn unregister_all(&mut self) -> Result<(), HotkeyError>;
    fn events(&self) -> std::sync::mpsc::Receiver<HotkeyId>;
}
// Canonical constructor (reconciled in Phase 1): returns a concrete handle so the
// single event receiver can be moved out exactly once and the OS queue pumped.
// The concrete impl (DesktopHotkeyManager) still implements HotkeyManager above.
pub fn new_hotkey_manager() -> Result<HotkeyHandle, HotkeyError>;
pub struct HotkeyHandle { /* wraps the runtime-selected impl */ }
impl HotkeyHandle {
    pub fn register(&mut self, id: HotkeyId, accelerator: &str) -> Result<(), HotkeyError>;
    pub fn unregister_all(&mut self) -> Result<(), HotkeyError>;
    pub fn take_events(&mut self) -> std::sync::mpsc::Receiver<HotkeyId>; // call once
    pub fn pump(&self);                                                   // drain OS queue → channel
}
```

## platform::textio (Phase 1)

```rust
#[derive(Debug, thiserror::Error)]
pub enum TextIoError { #[error("{0}")] Clipboard(String), #[error("{0}")] Inject(String) }

/// Baseline (Decision D3): clipboard save -> set text -> synthetic paste -> restore.
pub trait TextIo: Send + Sync {
    fn capture_selection(&self) -> Result<String, TextIoError>;     // synth copy then read clipboard
    fn replace_selection(&self, text: &str) -> Result<(), TextIoError>;
    fn insert_text(&self, text: &str) -> Result<(), TextIoError>;   // paste without prior copy
}
pub struct ClipboardTextIo;                                          // arboard + enigo; Wayland: uinput
impl ClipboardTextIo { pub fn new() -> Result<Self, TextIoError> }
pub fn new_text_io() -> Result<Box<dyn TextIo>, TextIoError>;
```

## platform::audio (Phase 2 capture, Phase 4 playback)

```rust
#[derive(Debug, thiserror::Error)]
pub enum AudioError { #[error("{0}")] Capture(String), #[error("{0}")] Playback(String) }

/// Captures mono f32 PCM at 16 kHz (Whisper's expected rate).
pub trait AudioCapture: Send {
    fn start(&mut self) -> Result<(), AudioError>;
    fn stop(&mut self) -> Result<CapturedAudio, AudioError>;
}
#[derive(Debug, Clone)]
pub struct CapturedAudio { pub samples: Vec<f32>, pub sample_rate: u32 }   // sample_rate == 16_000
pub fn new_audio_capture() -> Result<Box<dyn AudioCapture>, AudioError>;   // cpal

pub trait AudioPlayback: Send + Sync {                                     // Phase 4
    fn play_bytes(&self, audio: &[u8]) -> Result<(), AudioError>;          // encoded (mp3/wav) bytes
}
pub fn new_audio_playback() -> Result<Box<dyn AudioPlayback>, AudioError>;
```

## core::speech (Phase 2 transcription, Phase 4 TTS)

```rust
#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptionRequest { pub samples: Vec<f32>, pub sample_rate: u32, pub language: Option<String> }
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptionResult { pub text: String }

#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    #[error("http {0}: {1}")] Http(u16, String),
    #[error("network: {0}")] Network(String),
    #[error("inference: {0}")] Inference(String),
    #[error("invalid configuration: {0}")] Config(String),
}

#[async_trait::async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe(&self, request: TranscriptionRequest) -> Result<TranscriptionResult, SpeechError>;
}
pub struct RemoteTranscriber { /* api_key, base_url, model */ }            // Phase 2 (OpenAI / -compatible)
impl RemoteTranscriber { pub fn new(api_key: String, base_url: String, model: String) -> Self }

/// Engine selection persisted in Settings (see below). Factory lives in src-tauri
/// (it depends on both core and infer): builds Box<dyn Transcriber>.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SpeechEngineKind { OpenAi, OpenAiCompatible, WhisperLocal }       // WhisperLocal handled in Phase 3

// --- TTS (Phase 4) ---
#[derive(Debug, Clone, PartialEq)]
pub struct TtsRequest { pub text: String, pub voice: String, pub model: String }
#[async_trait::async_trait]
pub trait TextToSpeech: Send + Sync {
    async fn synthesize(&self, request: TtsRequest) -> Result<Vec<u8>, SpeechError>;   // returns encoded audio bytes
}
pub struct OpenAiTts { /* api_key, base_url */ }
impl OpenAiTts { pub fn new(api_key: String, base_url: String) -> Self }
```

## infer (Phase 3)

```rust
// local_transcriber.rs
pub struct LocalTranscriber { /* whisper context wrapping the model at a path */ }
impl LocalTranscriber { pub fn load(model_path: std::path::PathBuf) -> Result<Self, SpeechError> }
// impls core::speech::Transcriber via whisper-rs (whisper.cpp).

// model_manager.rs
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WhisperModel { pub id: String, pub display_name: String, pub url: String, pub size_bytes: u64 }
pub struct ModelManager { /* models_dir: PathBuf */ }
impl ModelManager {
    pub fn new(models_dir: std::path::PathBuf) -> Self;
    pub fn registry() -> Vec<WhisperModel>;                 // tiny..large-v3
    pub fn is_downloaded(&self, model_id: &str) -> bool;
    pub fn path_for(&self, model_id: &str) -> std::path::PathBuf;
    pub async fn download(&self, model_id: &str, on_progress: impl Fn(f64) + Send) -> Result<(), SpeechError>;
}
```

## Settings extensions (additive, each phase)

Each phase adds fields to `kea_core::settings::Settings` with `#[serde(default)]`
on the struct (already set in Phase 0) so older files still load. Bump
`schema_version` when adding. Canonical added fields:

- **Phase 1:** `pub openai_base_url: String` (default `"https://api.openai.com/v1"`),
  `pub rewrite_model: String` (default `"gpt-4o-mini"`),
  `pub presets: Vec<Preset>` (default `PromptCatalog::defaults()`),
  `pub active_preset_id: String`.
- **Phase 2:** `pub speech_engine: SpeechEngineKind` (default `OpenAi`),
  `pub speech_model: String` (default `"gpt-4o-transcribe"`),
  `pub speech_language: Option<String>` (default `None`).
- **Phase 3:** `pub local_model_id: String` (default `"base"`).
- **Phase 4:** `pub tts_voice: String` (default `"alloy"`),
  `pub tts_model: String` (default `"gpt-4o-mini-tts"`).

(Account names for secrets, used with `SecretStore`: `"openai"` for the API key.)

## Tauri command + event naming

- Commands: snake_case verbs already include `load_settings`, `save_settings`,
  `set_secret`, `has_secret`. Add per phase: `rewrite_selection` (P1),
  `start_dictation`/`stop_dictation` (P2), `list_models`/`download_model` (P3),
  `speak_selection` (P4).
- Events (Tauri `emit`): `"rewrite:status"`, `"speech:state"`,
  `"model:progress"`, `"tts:state"` with JSON payloads documented in each plan.

## Plan format (every phase plan MUST follow)

Mirror `2026-05-29-phase-0-foundation.md` exactly:

1. Header block: `# ... Implementation Plan`, the `> For agentic workers:`
   REQUIRED SUB-SKILL line, then **Goal / Architecture / Tech Stack / Reference spec**.
2. A **File Structure** section listing exact created/modified files + one-line responsibility each.
3. Numbered **Tasks**, each with a **Files:** block (Create/Modify/Test with exact paths)
   and bite-sized `- [ ]` steps: write failing test → run (expect FAIL) → minimal impl →
   run (expect PASS) → commit. Show COMPLETE real code in every code step — no placeholders,
   no "TBD", no "similar to above". Rust tests are in-file `#[cfg(test)]` modules; run with
   `cargo test -p <crate> <filter>`.
4. A **Phase N Acceptance** section and a **Self-Review Notes** section (spec coverage,
   type consistency vs. this contracts file, no-placeholder confirmation).

Use `async-trait`, `reqwest` (features `["json","rustls-tls"]`), `tokio` (in
`src-tauri`; core stays runtime-agnostic via the trait), `arboard`, `enigo`,
`global-hotkey`, `cpal`, `rodio` (playback), `whisper-rs`. Commits use
Conventional Commits and scope `app`/`core`/`platform`/`infer`/`ui`/`ci`.
