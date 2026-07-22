# KEA Phase 2 — Dictation (Speech-to-Text) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver push-to-talk dictation on macOS first (Windows/Linux follow as parallel platform tasks): a global hotkey starts mic capture, audio is buffered, the Dictation feature resolves a user-bound `SttEngine`, calls `SttEngine::transcribe(AudioPcm, SttOpts) -> Result<Transcript, EngineError>`, inserts the transcript at the cursor via `TextIo`, optionally post-processes through the rewrite LLM (`audio_refinement` mode), records the action in `data.db`, and surfaces level meter + state through Tauri events and a React speech overlay + Configuration/Features UI.

**Architecture:** Phase 0–1 shipped trait + registry scaffolding (`SttEngine` stub, `Feature`, `SlotResolver::resolve_llm`, `BindingRepo`, `CredentialSource`/`ProviderConfigSource` seam, `HttpClient`, SQLite stores, `Hotkeys`, `TextIo`). Phase 2 extends `SttEngine` with `transcribe`, adds `resolve_stt`, `platform/audio` (`AudioIo` mic capture), OpenAI-compatible HTTP STT + local Whisper (`whisper-rs` via `kea-infer`, feature-gated), `DictationFeature`, and thin Tauri wiring. Consumers depend on traits; `src-tauri` is the only composition root. Dependency inversion from Phase 1 stands: engines never depend on `kea-core`.

**Tech Stack:** Rust (edition 2021), Tauri 2.x, `cpal` (mic capture), `reqwest` + `wiremock` (HTTP STT tests), `whisper-rs` (optional, behind `whisper` cargo feature — D5), `sqlx`, `tokio`, `async-trait`, `serde`/`serde_json`, `keyring`, Vite + React + TypeScript (D10).

## Global Constraints

- **Product name:** `KEA` everywhere (`kea-*` crates, `ai.kea.app`). _(D13.)_
- **Plugin model:** internal trait + registry, compiled in. No dynamic loading. _(D1, D2.)_
- **Storage boundary (D9):** `config.db` = settings, bindings, hotkey bindings, provider config (base URL + default model — **not** API keys), dictation settings, installed-model metadata; `data.db` = actions; **keyring = credentials only** via `CredentialStore` / `CredentialSource`. DB rows store `engine_id`, `model`, `provider_ref` references — never secrets.
- **Web UI (D10):** React pages composed from a shared component library; Rust plugins expose typed Tauri commands/events only.
- **Async:** all engine/platform I/O trait methods are `async` (`async-trait`).
- **TDD:** every code task is test-first. Rust async tests use `#[tokio::test]`. Store tests use `sqlite::memory:`; HTTP engine tests use `wiremock` or injected `HttpClient` — **never hit real OpenAI in unit tests**.
- **No real mic in unit tests:** `AudioIo` is exercised via `FakeAudioIo` in feature/orchestration tests; macOS `cpal` impl is compile + manual acceptance only (Microphone permission).
- **No real network in unit tests:** STT HTTP engines use `wiremock`; model download tests mock the HTTP layer or inject a `DownloadTransport` trait.
- **Whisper feature gate:** `whisper-rs` / whisper.cpp build is behind `features = ["whisper"]` on `kea-engines` and `kea-infer`. Default `cargo test --workspace` and CI **do not** enable this feature — no whisper.cpp compile in default pipeline. Whisper unit tests use a `WhisperInference` trait mock; integration with a real GGUF model is manual only.
- **macOS-first:** Tasks through macOS `AudioIo` + dictation E2E must pass before treating Phase 2 done on the primary dev machine. Windows/Linux `AudioIo` impls are clearly labeled **parallel per-OS** and may land after macOS E2E.
- **Targets:** code compiles on macOS, Windows, Linux; CI runs `cargo test --workspace` (without `--features whisper`) on all three.
- **Commits:** frequent conventional commits, one per task minimum. Use `git commit --no-verify` when the legacy Vox FluidAudio pre-commit hook blocks unrelated paths.

---

## File Structure

```
kea/
├─ Cargo.toml                              # add cpal workspace dep; optional whisper-rs
├─ crates/
│  ├─ core/
│  │  ├─ migrations/config/0003_dictation.sql
│  │  └─ src/
│  │     ├─ resolve.rs                     # add resolve_stt (mirror resolve_llm)
│  │     └─ dictation/
│  │        ├─ mod.rs
│  │        └─ settings.rs                  # DictationSettings repo (config.db)
│  ├─ engines/
│  │  ├─ Cargo.toml                         # optional feature whisper = [kea-infer/whisper]
│  │  └─ src/
│  │     ├─ http.rs                         # extend: post_multipart for STT
│  │     ├─ traits.rs                       # AudioPcm, SttOpts, Transcript; SttEngine::transcribe
│  │     ├─ registry.rs                     # register_stt, list_stt_ids, stt()
│  │     ├─ stt/
│  │     │  ├─ mod.rs
│  │     │  ├─ openai.rs                    # OpenAiSttEngine (HTTP /audio/transcriptions)
│  │     │  └─ whisper.rs                   # [feature whisper] WhisperSttEngine
│  │     ├─ noop.rs                         # NoopSttEngine for tests
│  │     └─ lib.rs                          # register_phase2_stt_engines()
│  ├─ infer/
│  │  ├─ Cargo.toml                         # feature whisper = [dep:whisper-rs]
│  │  └─ src/
│  │     ├─ lib.rs
│  │     ├─ registry.rs                     # WhisperModelEntry catalog (GGUF ids, URLs)
│  │     ├─ storage.rs                      # on-disk paths under app data dir
│  │     ├─ download.rs                     # ModelDownloader + progress callback
│  │     └─ whisper.rs                      # [feature whisper] WhisperInference impl
│  ├─ features/
│  │  └─ src/
│  │     ├─ dictation.rs                    # DictationFeature + run_dictation()
│  │     └─ lib.rs
│  └─ platform/
│     └─ src/
│        ├─ lib.rs                            # new_audio_io()
│        ├─ audio/
│        │  ├─ mod.rs                        # AudioIo trait, PcmBuffer, format helpers
│        │  ├─ macos.rs                      # cpal mic capture
│        │  └─ stub.rs                        # non-macOS stub (compile-only)
│        └─ textio/mod.rs                    # add insert_at_cursor (reuse D4 paste)
├─ src-tauri/src/
│  ├─ main.rs                                # register_phase2_stt_engines, DictationFeature, PTT hotkey
│  ├─ commands.rs                            # stt/model/dictation commands
│  └─ events.rs                              # dictation:state, dictation:level, model:download:progress
└─ ui/src/
   ├─ api.ts                                 # STT/dictation/model typed wrappers
   ├─ App.tsx                                 # speech overlay listeners
   ├─ components/
   │  ├─ LevelMeter.tsx
   │  ├─ SpeechOverlay.tsx
   │  ├─ ModelManager.tsx
   │  └─ DictationPanel.tsx
   └─ pages/
      ├─ ConfigurationPage.tsx               # + ModelManager
      └─ FeaturesPage.tsx                    # + DictationPanel
```

---

### Task 1: Workspace + crate dependencies for Phase 2

**Files:**
- Modify: `Cargo.toml` (workspace deps)
- Modify: `crates/platform/Cargo.toml`, `crates/engines/Cargo.toml`, `crates/infer/Cargo.toml`, `crates/features/Cargo.toml`, `src-tauri/Cargo.toml`

**Interfaces:**
- Produces: workspace dep `cpal`; `kea-infer` wired into workspace; `kea-engines` optional feature `whisper = ["kea-infer/whisper", "dep:whisper-rs"]`; default builds omit `whisper-rs`.

- [ ] **Step 1: Write the failing test**

In `crates/platform/src/lib.rs` extend the existing test:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpal_is_linked() {
        let _ = std::any::type_name::<cpal::Sample>();
    }

    #[test]
    fn platform_constructors_do_not_panic() {
        let _hotkeys = new_hotkeys();
        let _text_io = new_text_io();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-platform cpal_is_linked`
Expected: FAIL — `cpal` not found.

- [ ] **Step 3: Add workspace + crate dependencies**

Root `Cargo.toml` `[workspace.dependencies]` append:

```toml
cpal = "0.15"
```

`crates/platform/Cargo.toml`:

```toml
cpal.workspace = true
```

`crates/infer/Cargo.toml`:

```toml
[features]
default = []
whisper = ["dep:whisper-rs"]

[dependencies]
tokio.workspace = true
serde_json.workspace = true
tempfile.workspace = true

[dependencies.whisper-rs]
version = "0.13"
optional = true
```

`crates/engines/Cargo.toml`:

```toml
kea-infer = { path = "../infer", optional = true }

[features]
default = []
whisper = ["kea-infer/whisper", "dep:whisper-rs"]

[dependencies.whisper-rs]
version = "0.13"
optional = true
```

`crates/features/Cargo.toml` — ensure `kea-infer` is **not** a direct dep (dictation talks to STT via `EngineRegistry` only).

`src-tauri/Cargo.toml`:

```toml
kea-infer = { path = "../crates/infer" }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-platform cpal_is_linked`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/platform crates/engines crates/infer src-tauri/Cargo.toml
git commit --no-verify -m "feat(phase2): add cpal and optional whisper-rs deps"
```

---

### Task 2: Extend `SttEngine` trait — `AudioPcm`, `SttOpts`, `Transcript`, `transcribe`

**Files:**
- Modify: `crates/engines/src/traits.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub struct AudioPcm {
      pub samples: Vec<f32>,       // mono, normalized [-1.0, 1.0]
      pub sample_rate_hz: u32,     // e.g. 16000 for Whisper; engines may resample
  }

  #[derive(Debug, Clone, Default)]
  pub struct SttOpts {
      pub model: Option<String>,           // binding model or engine default
      pub language: Option<String>,      // ISO-639-1, None = auto
      pub provider_ref: Option<String>,   // for HTTP engines
  }

  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Transcript {
      pub text: String,
  }

  #[async_trait]
  pub trait SttEngine: Send + Sync {
      fn id(&self) -> &str;
      fn capabilities(&self) -> EngineCaps;
      async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError>;
  }
  ```

- [ ] **Step 1: Write the failing test**

Append to `crates/engines/src/traits.rs`:

```rust
#[cfg(test)]
mod stt_types_tests {
    use super::*;

    #[test]
    fn audio_pcm_holds_mono_samples() {
        let pcm = AudioPcm {
            samples: vec![0.0, 0.5, -0.5],
            sample_rate_hz: 16_000,
        };
        assert_eq!(pcm.samples.len(), 3);
        assert_eq!(pcm.sample_rate_hz, 16_000);
    }

    #[test]
    fn transcript_roundtrips_json() {
        let t = Transcript { text: "hello world".into() };
        let json = serde_json::to_string(&t).unwrap();
        let back: Transcript = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
}
```

Add `Serialize` to `Transcript` for Tauri events.

- [ ] **Step 2: Run test — FAIL**

Run: `cargo test -p kea-engines stt_types_tests`
Expected: FAIL — types not defined.

- [ ] **Step 3: Implement types + extend trait**

Replace the `SttEngine` stub with the full trait above. Keep `TtsEngine` stub unchanged.

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(engines): SttEngine transcribe types and trait method"
```

---

### Task 3: `NoopSttEngine` + extend `EngineRegistry` for STT

**Files:**
- Modify: `crates/engines/src/noop.rs`, `crates/engines/src/registry.rs`, `crates/engines/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct NoopSttEngine;
  // id == "noop-stt"; transcribe returns Transcript { text: format!("heard: {} samples", audio.samples.len()) }

  impl EngineRegistry {
      pub fn register_stt(&mut self, e: Arc<dyn SttEngine>);
      pub fn stt(&self, id: &str) -> Option<Arc<dyn SttEngine>>;
      pub fn list_stt_ids(&self) -> Vec<String>;
  }
  ```

- [ ] **Step 1: Write the failing test**

`crates/engines/src/registry.rs`:

```rust
#[cfg(test)]
mod stt_registry_tests {
    use super::*;
    use crate::noop::NoopSttEngine;
    use crate::traits::{AudioPcm, SttOpts, SttEngine};
    use async_trait::async_trait;
    use std::sync::Arc;

    #[tokio::test]
    async fn register_and_transcribe_noop_stt() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(NoopSttEngine));
        assert_eq!(reg.list_stt_ids(), vec!["noop-stt".to_string()]);
        let engine = reg.stt("noop-stt").unwrap();
        let out = engine
            .transcribe(
                AudioPcm { samples: vec![0.1; 100], sample_rate_hz: 16_000 },
                SttOpts::default(),
            )
            .await
            .unwrap();
        assert!(out.text.contains("100"));
    }
}
```

- [ ] **Step 2: Run test — FAIL**

- [ ] **Step 3: Implement `NoopSttEngine` + registry STT map**

`crates/engines/src/noop.rs` append `NoopSttEngine` impl.

`crates/engines/src/registry.rs`:

```rust
use crate::traits::SttEngine;

#[derive(Default)]
pub struct EngineRegistry {
    llm: HashMap<String, Arc<dyn LlmEngine>>,
    stt: HashMap<String, Arc<dyn SttEngine>>,
}
```

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(engines): EngineRegistry STT slot and NoopSttEngine"
```

---

### Task 4: `SlotResolver::resolve_stt` (mirror `resolve_llm`)

**Files:**
- Modify: `crates/core/src/resolve.rs`

**Interfaces:**
- Produces: `pub async fn resolve_stt(&self, feature_id: &str, slot: &str) -> Result<Resolution, KeaError>` — identical logic to `resolve_llm` but uses `engines.list_stt_ids()` and `engines.stt(&b.engine_id)`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn unbound_single_stt_engine_autobinds() {
    let (bindings, mut reg) = setup().await;
    reg.register_stt(Arc::new(NoopSttEngine));
    let r = SlotResolver::new(&reg, &bindings).resolve_stt("dictation", "stt").await.unwrap();
    assert!(matches!(r, Resolution::Bound(id) if id == "noop-stt"));
}

#[tokio::test]
async fn bound_stt_to_missing_engine_is_unresolvable() {
    let (bindings, reg) = setup().await;
    bindings.set("dictation", "stt", Binding {
        engine_id: "ghost-stt".into(), model: None, provider_ref: None,
    }).await.unwrap();
    let r = SlotResolver::new(&reg, &bindings).resolve_stt("dictation", "stt").await.unwrap();
    assert!(matches!(r, Resolution::Unresolvable));
}
```

Import `NoopSttEngine` from `kea_engines::noop::NoopSttEngine`; extend `setup()` helper's `EngineRegistry` usage.

- [ ] **Step 2: Run test — FAIL**

Run: `cargo test -p kea-core resolve_stt`

- [ ] **Step 3: Implement `resolve_stt`**

Copy `resolve_llm` body; swap `llm` → `stt`, `list_llm_ids` → `list_stt_ids`.

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(core): SlotResolver resolve_stt"
```

---

### Task 5: Extend `HttpClient` — `post_multipart` for STT

**Files:**
- Modify: `crates/engines/src/http.rs`, `crates/engines/Cargo.toml` (enable `reqwest` `multipart` feature)

**Interfaces:**
- Produces:
  ```rust
  pub struct MultipartPart {
      pub name: String,
      pub filename: Option<String>,
      pub content_type: Option<String>,
      pub data: Vec<u8>,
  }

  #[async_trait]
  pub trait HttpClient: Send + Sync {
      async fn post_json(/* existing */);
      async fn post_multipart(
          &self, url: &str, bearer: &str, parts: Vec<MultipartPart>,
      ) -> Result<(u16, String), EngineError>;
  }
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn posts_multipart_audio_transcription() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"text":"hello"}"#))
        .mount(&server).await;

    let http = ReqwestHttpClient::new();
    let (status, body) = http.post_multipart(
        &format!("{}/v1/audio/transcriptions", server.uri()),
        "sk-test",
        vec![
            MultipartPart {
                name: "file".into(),
                filename: Some("audio.wav".into()),
                content_type: Some("audio/wav".into()),
                data: vec![0x52, 0x49, 0x46, 0x46], // minimal stub bytes
            },
            MultipartPart {
                name: "model".into(),
                filename: None,
                content_type: None,
                data: b"whisper-1".to_vec(),
            },
        ],
    ).await.unwrap();
    assert_eq!(status, 200);
    assert!(body.contains("hello"));
}
```

- [ ] **Step 2: Run test — FAIL**

- [ ] **Step 3: Implement `post_multipart` on `ReqwestHttpClient`**

Use `reqwest::multipart::Form`; add `multipart` to workspace `reqwest` features.

- [ ] **Step 4: Run test — PASS** (wiremock — no real network)

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(engines): HttpClient post_multipart for STT"
```

---

### Task 6: `pcm_to_wav_bytes` helper (pure, unit-tested)

**Files:**
- Create: `crates/engines/src/stt/audio.rs`, `crates/engines/src/stt/mod.rs`
- Modify: `crates/engines/src/lib.rs`

**Interfaces:**
- Produces: `pub fn pcm_to_wav_bytes(pcm: &AudioPcm) -> Result<Vec<u8>, EngineError>` — mono 16-bit PCM WAV header + samples.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::AudioPcm;

    #[test]
    fn wav_has_riff_header_and_correct_data_size() {
        let pcm = AudioPcm {
            samples: vec![0.0, 1.0, -1.0],
            sample_rate_hz: 16_000,
        };
        let wav = pcm_to_wav_bytes(&pcm).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        // 3 samples * 2 bytes = 6 bytes of PCM data
        let data_chunk_size = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(data_chunk_size, 6);
    }
}
```

- [ ] **Step 2–5:** implement minimal WAV encoder (no external crate), export, commit.

```bash
git commit --no-verify -m "feat(engines): pcm_to_wav_bytes for HTTP STT upload"
```

---

### Task 7: `OpenAiSttEngine` implementing `SttEngine`

**Files:**
- Create: `crates/engines/src/stt/openai.rs`
- Modify: `crates/engines/src/stt/mod.rs`, `crates/engines/src/lib.rs`

**Interfaces:**
- Consumes: `HttpClient::post_multipart`, `CredentialSource`, `ProviderConfigSource`, `pcm_to_wav_bytes`.
- Produces: `OpenAiSttEngine { http, credentials, configs, provider_ref: String }` with `id() == "openai-stt"`.

```rust
#[async_trait]
impl SttEngine for OpenAiSttEngine {
    fn id(&self) -> &str { "openai-stt" }
    fn capabilities(&self) -> EngineCaps {
        EngineCaps { models: vec!["whisper-1".into(), "gpt-4o-mini-transcribe".into()] }
    }
    async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError> {
        let provider_ref = opts.provider_ref.as_deref().unwrap_or(&self.provider_ref);
        let api_key = self.credentials.api_key(provider_ref).await
            .ok_or_else(|| EngineError::Other("missing api key".into()))?;
        let cfg = self.configs.config(provider_ref).await.unwrap_or(ProviderConfig {
            base_url: "https://api.openai.com/v1".into(),
            default_model: "whisper-1".into(),
        });
        let model = opts.model.as_deref().unwrap_or(&cfg.default_model);
        let wav = pcm_to_wav_bytes(&audio)?;
        let url = format!("{}/audio/transcriptions", cfg.base_url.trim_end_matches('/'));
        let parts = vec![
            MultipartPart { name: "file".into(), filename: Some("audio.wav".into()),
                content_type: Some("audio/wav".into()), data: wav },
            MultipartPart { name: "model".into(), filename: None, content_type: None,
                data: model.as_bytes().to_vec() },
        ];
        let (status, text) = self.http.post_multipart(&url, &api_key, parts).await?;
        if status != 200 {
            return Err(EngineError::Other(format!("http {status}: {text}")));
        }
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let content = parsed["text"].as_str()
            .ok_or_else(|| EngineError::Other("missing text field".into()))?;
        Ok(Transcript { text: content.to_string() })
    }
}
```

- [ ] **Step 1: Write the failing test** (reuse `FakeCredentials`/`FakeConfigs` pattern from `llm/openai.rs` tests):

```rust
#[tokio::test]
async fn transcribes_against_mock_openai_stt() {
    let server = MockServer::start().await;
    Mock::given(method("POST")).and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"text":"dictated text"}"#))
        .mount(&server).await;
    // wire engine with FakeCredentials { openai: sk-test }, FakeConfigs { base_url: server.uri() }
    let engine = OpenAiSttEngine { /* ... */ provider_ref: "openai".into() };
    let out = engine.transcribe(
        AudioPcm { samples: vec![0.0; 1600], sample_rate_hz: 16_000 },
        SttOpts { model: None, language: None, provider_ref: Some("openai".into()) },
    ).await.unwrap();
    assert_eq!(out.text, "dictated text");
}
```

- [ ] **Step 2–5:** implement, test PASS, commit.

```bash
git commit --no-verify -m "feat(engines): OpenAiSttEngine with wiremock tests"
```

---

### Task 8: `register_phase2_stt_engines()` (HTTP always; Whisper behind feature)

**Files:**
- Modify: `crates/engines/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn register_phase2_stt_engines(
      reg: &mut EngineRegistry,
      http: Arc<dyn HttpClient>,
      credentials: Arc<dyn CredentialSource>,
      configs: Arc<dyn ProviderConfigSource>,
  ) {
      reg.register_stt(Arc::new(OpenAiSttEngine {
          http: http.clone(), credentials: credentials.clone(),
          configs: configs.clone(), provider_ref: "openai".into(),
      }));
      #[cfg(feature = "whisper")]
      reg.register_stt(Arc::new(WhisperSttEngine::new(/* infer handle */)));
  }
  ```

- [ ] **Step 1: Write the failing test** (default features — no whisper):

```rust
#[test]
fn registers_openai_stt_engine() {
    let mut reg = EngineRegistry::default();
    register_phase2_stt_engines(
        &mut reg,
        Arc::new(ReqwestHttpClient::new()),
        Arc::new(FakeCredentials),
        Arc::new(FakeConfigs),
    );
    let ids = reg.list_stt_ids();
    assert!(ids.contains(&"openai-stt".to_string()));
    #[cfg(feature = "whisper")]
    assert!(ids.contains(&"whisper".to_string()));
    #[cfg(not(feature = "whisper"))]
    assert!(!ids.contains(&"whisper".to_string()));
}
```

- [ ] **Step 2–5:** implement (Whisper registration stubbed until Task 19), commit.

```bash
git commit --no-verify -m "feat(engines): register_phase2_stt_engines"
```

---

### Task 9: `config.db` migration — dictation settings keys

**Files:**
- Create: `crates/core/migrations/config/0003_dictation.sql`
- Create: `crates/core/src/dictation/settings.rs`, `crates/core/src/dictation/mod.rs`
- Modify: `crates/core/src/lib.rs`

**Interfaces:**
- Migration adds no new tables (settings live in existing `settings` KV table); documents canonical keys:
  - `dictation.post_process` → `"true"` | `"false"` (optional LLM `audio_refinement`)
  - `dictation.active_model` → Whisper model id when using local engine
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct DictationSettings {
      pub post_process: bool,
      pub active_model: Option<String>,
  }
  pub struct DictationSettingsRepo { settings: SettingsRepo }
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn dictation_settings_roundtrip() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    run_config_migrations(&pool).await.unwrap();
    let repo = DictationSettingsRepo::new(SettingsRepo::new(pool));
    let cfg = DictationSettings { post_process: true, active_model: Some("ggml-base.en".into()) };
    repo.set(&cfg).await.unwrap();
    assert_eq!(repo.get().await.unwrap(), cfg);
}
```

- [ ] **Step 2: Run test — FAIL** (migration file may be empty placeholder — migrator still runs)

- [ ] **Step 3: Write migration** (`0003_dictation.sql` can be a comment-only migration marking the schema version bump; settings are KV).

- [ ] **Step 4–5:** implement repo, PASS, commit.

```bash
git commit --no-verify -m "feat(core): DictationSettings repo on config.db"
```

---

### Task 10: Platform `AudioIo` trait + `PcmBuffer` types

**Files:**
- Create: `crates/platform/src/audio/mod.rs`
- Modify: `crates/platform/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq)]
  pub struct PcmFrame {
      pub samples: Vec<f32>,
      pub sample_rate_hz: u32,
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
  pub enum DictationState {
      Idle,
      Listening,
      Processing,
  }

  pub enum AudioIoError { #[error("{0}")] Other(String) }

  #[async_trait]
  pub trait AudioIo: Send + Sync {
      /// Push-to-talk: begin mic capture; frames arrive via `frame_rx`.
      async fn start_mic(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError>;
      /// Stop capture and return the full buffered mono PCM at the device's native rate.
      async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError>;
      /// RMS level of the most recent frame in [0.0, 1.0] for UI meter; 0.0 when idle.
      fn current_level(&self) -> f32;
      fn state(&self) -> DictationState;
  }

  pub fn new_audio_io() -> Box<dyn AudioIo>;
  ```

> **Deferred (Phase 3/4):** `capture_system`, `play` from spec §4.2 — Meetings loopback (D14) and TTS playback (D11) are not Phase 2.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod audio_trait_tests {
    use super::*;
    use async_trait::async_trait;

    struct FakeAudioIo {
        state: DictationState,
        buffered: PcmFrame,
    }

    #[async_trait]
    impl AudioIo for FakeAudioIo {
        async fn start_mic(&mut self) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.state = DictationState::Listening;
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }
        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            self.state = DictationState::Idle;
            Ok(self.buffered.clone())
        }
        fn current_level(&self) -> f32 { 0.42 }
        fn state(&self) -> DictationState { self.state }
    }

    #[tokio::test]
    async fn fake_audio_io_returns_buffered_pcm() {
        let mut io = FakeAudioIo {
            state: DictationState::Idle,
            buffered: PcmFrame { samples: vec![0.1, 0.2], sample_rate_hz: 48_000 },
        };
        let _rx = io.start_mic().await.unwrap();
        assert_eq!(io.state(), DictationState::Listening);
        let pcm = io.stop_mic().await.unwrap();
        assert_eq!(pcm.samples.len(), 2);
    }
}
```

- [ ] **Step 2–5:** implement trait module, `new_audio_io()` stub factory, commit.

```bash
git commit --no-verify -m "feat(platform): AudioIo trait and PcmFrame types"
```

---

### Task 11: Pure audio helpers — resample + RMS level

**Files:**
- Create: `crates/platform/src/audio/util.rs`
- Modify: `crates/platform/src/audio/mod.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn resample_linear(frame: &PcmFrame, target_rate_hz: u32) -> PcmFrame;
  pub fn rms_level(samples: &[f32]) -> f32;  // 0.0..1.0
  pub fn accumulate_frames(frames: &[PcmFrame]) -> PcmFrame;
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn resample_halves_sample_count_when_halving_rate() {
    let frame = PcmFrame {
        samples: (0..100).map(|i| i as f32 / 100.0).collect(),
        sample_rate_hz: 48_000,
    };
    let out = resample_linear(&frame, 24_000);
    assert_eq!(out.sample_rate_hz, 24_000);
    assert_eq!(out.samples.len(), 50);
}

#[test]
fn rms_silence_is_zero() {
    assert_eq!(rms_level(&[0.0, 0.0, 0.0]), 0.0);
}

#[test]
fn rms_full_scale_is_one() {
    assert!((rms_level(&[1.0, -1.0, 1.0]) - 1.0).abs() < 0.01);
}
```

- [ ] **Step 2–5:** implement linear resampler (sufficient for Phase 2), commit.

```bash
git commit --no-verify -m "feat(platform): audio resample and RMS helpers"
```

---

### Task 12: macOS `AudioIo` via `cpal`

**Files:**
- Create: `crates/platform/src/audio/macos.rs`
- Modify: `crates/platform/src/audio/mod.rs`, `crates/platform/src/lib.rs`

**Interfaces:**
- Produces: `MacAudioIo` implementing `AudioIo`; `new_audio_io()` returns it on `target_os = "macos"`.

> **Permissions:** Microphone access required on macOS. Unit tests cover **parser/state logic only** — no real mic in CI. Manual acceptance verifies capture.

- [ ] **Step 1: Write the failing test (state machine, no cpal)**

```rust
#[test]
fn dictation_state_starts_idle() {
    let io = MacAudioIo::new_for_test();
    assert_eq!(io.state(), DictationState::Idle);
    assert_eq!(io.current_level(), 0.0);
}
```

Expose `new_for_test()` that skips device open (internal `Option<cpal::Stream>` = None).

- [ ] **Step 2: Run test — FAIL**

- [ ] **Step 3: Implement `MacAudioIo`**

- Open default input device via `cpal::default_host().default_input_device()`.
- On `start_mic`: build input stream (`f32` samples), push `PcmFrame` chunks to `mpsc`, update `current_level` via `rms_level`.
- On `stop_mic`: drop stream, concatenate buffered frames with `accumulate_frames`.
- Non-test constructor opens real device.

`crates/platform/src/lib.rs`:

```rust
pub mod audio;
pub use audio::{AudioIo, AudioIoError, DictationState, PcmFrame, new_audio_io};

pub fn new_audio_io() -> Box<dyn audio::AudioIo> {
    #[cfg(target_os = "macos")]
    { return Box::new(audio::macos::MacAudioIo::new()); }
    #[cfg(not(target_os = "macos"))]
    { Box::new(audio::stub::StubAudioIo::new()) }
}
```

- [ ] **Step 4: Run state test — PASS** on all OSes (macOS logic compiles everywhere with cfg).

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(platform): macOS AudioIo mic capture via cpal"
```

---

### Task 13: **[PARALLEL — Windows]** `AudioIo` stub → WASAPI via cpal

**Files:**
- Create: `crates/platform/src/audio/windows.rs` (or share cpal impl with cfg)
- Modify: `crates/platform/src/lib.rs`

**Interfaces:**
- Produces: `WindowsAudioIo` — cpal uses WASAPI backend on Windows; same struct as macOS with `target_os` module split if needed.

- [ ] **TDD:** state-machine unit test only; manual mic check on Windows hardware.
- [ ] **Commit:**

```bash
git commit --no-verify -m "feat(platform): Windows AudioIo via cpal"
```

---

### Task 14: **[PARALLEL — Linux]** `AudioIo` via cpal (ALSA/Pulse/Pipewire)

**Files:**
- Create: `crates/platform/src/audio/linux.rs`, `crates/platform/src/audio/stub.rs`
- Modify: `crates/platform/src/lib.rs`

**Interfaces:**
- Produces: compile-only stub on non-macOS until parallel task lands; then cpal backend.

- [ ] **TDD:** `StubAudioIo::start_mic` returns `AudioIoError::Other("audio not implemented on this platform".into())` — feature tests use `FakeAudioIo`, not stub.
- [ ] **Commit:**

```bash
git commit --no-verify -m "feat(platform): Linux AudioIo via cpal"
```

---

### Task 15: `TextIo::insert_at_cursor` (reuse D4 paste path)

**Files:**
- Modify: `crates/platform/src/textio/mod.rs`, `crates/platform/src/textio/macos.rs`, `crates/platform/src/textio/stub.rs`

**Interfaces:**
- Produces:
  ```rust
  #[async_trait]
  pub trait TextIo: Send + Sync {
      async fn capture_selection(&self) -> Result<String, TextIoError>;
      async fn replace(&self, text: &str) -> Result<(), TextIoError>;
      /// Insert text at the caret without requiring a prior selection (dictation).
      async fn insert_at_cursor(&self, text: &str) -> Result<(), TextIoError>;
  }
  ```

- [ ] **Step 1: Write the failing test** (extend existing `FakeTextIo` in `textio/mod.rs` tests):

```rust
#[tokio::test]
async fn fake_textio_inserts_at_cursor() {
    let fake = FakeTextIo { selection: String::new(), replaced: Mutex::new(None) };
    fake.insert_at_cursor("dictated").await.unwrap();
    assert_eq!(fake.replaced.lock().unwrap().as_deref(), Some("dictated"));
}
```

Extend `FakeTextIo` to record inserts in the same `replaced` mutex.

- [ ] **Step 3: Implement on `MacTextIo`**

Same as `replace` but skip `capture_selection` — clipboard save → set text → synthetic paste → restore (D4).

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(platform): TextIo insert_at_cursor for dictation"
```

---

### Task 16: `kea-infer` — Whisper model registry (no network)

**Files:**
- Create: `crates/infer/src/registry.rs`, `crates/infer/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct WhisperModelEntry {
      pub id: String,           // e.g. "ggml-base.en"
      pub display_name: String,
      pub url: String,          // Hugging Face / official GGUF URL
      pub size_bytes: u64,
  }

  pub struct ModelRegistry;
  impl ModelRegistry {
      pub fn whisper_catalog() -> Vec<WhisperModelEntry>;
      pub fn find_whisper(id: &str) -> Option<WhisperModelEntry>;
  }
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn catalog_includes_base_en() {
    let models = ModelRegistry::whisper_catalog();
    assert!(models.iter().any(|m| m.id == "ggml-base.en"));
    let found = ModelRegistry::find_whisper("ggml-base.en").unwrap();
    assert!(found.url.contains("ggml-base.en"));
}
```

- [ ] **Step 2–5:** implement static catalog (base, small, medium — match D5 sensible defaults), commit.

```bash
git commit --no-verify -m "feat(infer): Whisper model registry catalog"
```

---

### Task 17: `kea-infer` — on-disk storage paths

**Files:**
- Create: `crates/infer/src/storage.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct ModelStorage {
      pub root: PathBuf,  // e.g. {app_data}/models/whisper
  }
  impl ModelStorage {
      pub fn new(root: PathBuf) -> Self;
      pub fn path_for(&self, model_id: &str) -> PathBuf;
      pub fn is_installed(&self, model_id: &str) -> bool;
      pub fn installed_models(&self) -> Vec<String>;
  }
  ```

- [ ] **Step 1: Write the failing test** (uses `tempfile::tempdir()`):

```rust
#[test]
fn path_for_model_is_stable() {
    let dir = tempfile::tempdir().unwrap();
    let storage = ModelStorage::new(dir.path().to_path_buf());
    let path = storage.path_for("ggml-base.en");
    assert!(path.ends_with("ggml-base.en.gguf"));
    assert!(!storage.is_installed("ggml-base.en"));
    std::fs::write(&path, b"fake").unwrap();
    assert!(storage.is_installed("ggml-base.en"));
    assert_eq!(storage.installed_models(), vec!["ggml-base.en".to_string()]);
}
```

- [ ] **Step 2–5:** implement, commit.

```bash
git commit --no-verify -m "feat(infer): ModelStorage paths for Whisper GGUF"
```

---

### Task 18: `kea-infer` — `ModelDownloader` with progress callback (mocked HTTP)

**Files:**
- Create: `crates/infer/src/download.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq, Serialize)]
  pub struct DownloadProgress {
      pub model_id: String,
      pub bytes_received: u64,
      pub bytes_total: u64,
  }

  #[async_trait]
  pub trait DownloadTransport: Send + Sync {
      async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, InferError>;
  }

  pub struct ModelDownloader {
      transport: Arc<dyn DownloadTransport>,
      storage: ModelStorage,
  }
  impl ModelDownloader {
      pub async fn download_whisper(
          &self,
          model_id: &str,
          on_progress: impl Fn(DownloadProgress) + Send + Sync,
      ) -> Result<PathBuf, InferError>;
  }
  ```

- [ ] **Step 1: Write the failing test**

```rust
struct FakeTransport { data: Vec<u8> }
#[async_trait]
impl DownloadTransport for FakeTransport {
    async fn get_bytes(&self, _url: &str) -> Result<Vec<u8>, InferError> {
        Ok(self.data.clone())
    }
}

#[tokio::test]
async fn downloader_writes_file_and_reports_progress() {
    let dir = tempfile::tempdir().unwrap();
    let storage = ModelStorage::new(dir.path().to_path_buf());
    let dl = ModelDownloader {
        transport: Arc::new(FakeTransport { data: vec![1, 2, 3, 4] }),
        storage,
    };
    let progress = Arc::new(Mutex::new(Vec::new()));
    let p2 = progress.clone();
    let path = dl.download_whisper("ggml-base.en", move |p| {
        p2.lock().unwrap().push(p);
    }).await.unwrap();
    assert!(path.exists());
    assert_eq!(std::fs::read(&path).unwrap(), vec![1, 2, 3, 4]);
    assert!(!progress.lock().unwrap().is_empty());
}
```

- [ ] **Step 2–5:** implement (single-shot download OK for Phase 2; no resume required), commit.

```bash
git commit --no-verify -m "feat(infer): ModelDownloader with injectable transport"
```

---

### Task 19: `WhisperInference` trait boundary + `WhisperSttEngine` (feature `whisper`)

**Files:**
- Create: `crates/infer/src/whisper.rs` (behind `feature = "whisper"`)
- Create: `crates/engines/src/stt/whisper.rs` (behind `feature = "whisper"`)
- Modify: `crates/engines/src/lib.rs` (`register_phase2_stt_engines`)

**Interfaces:**
- Produces (in `kea-infer`, always compiled — mockable without whisper-rs):
  ```rust
  #[async_trait]
  pub trait WhisperInference: Send + Sync {
      async fn transcribe_pcm(&self, pcm: AudioPcm, model_path: &Path) -> Result<String, InferError>;
  }
  ```

- Produces (in `kea-engines`, feature-gated):
  ```rust
  pub struct WhisperSttEngine {
      inference: Arc<dyn WhisperInference>,
      storage: Arc<ModelStorage>,
  }
  // id == "whisper"
  // capabilities().models = ModelRegistry::whisper_catalog().iter().map(|m| m.id.clone()).collect()
  // transcribe: resolve model from opts.model or error if not installed; resample to 16kHz; call inference
  ```

> **Default CI/tests:** `WhisperSttEngine` unit test lives in `engines` with a `FakeWhisperInference` — **no `whisper-rs` link required**:

```rust
struct FakeWhisperInference;
#[async_trait]
impl WhisperInference for FakeWhisperInference {
    async fn transcribe_pcm(&self, pcm: AudioPcm, _model_path: &Path) -> Result<String, InferError> {
        Ok(format!("whisper heard {} samples", pcm.samples.len()))
    }
}

#[tokio::test]
async fn whisper_engine_uses_inference_trait() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
    let model_path = storage.path_for("ggml-base.en");
    std::fs::write(&model_path, b"x").unwrap();
    let engine = WhisperSttEngine {
        inference: Arc::new(FakeWhisperInference),
        storage: storage.clone(),
    };
    let out = engine.transcribe(
        AudioPcm { samples: vec![0.0; 16000], sample_rate_hz: 16_000 },
        SttOpts { model: Some("ggml-base.en".into()), ..Default::default() },
    ).await.unwrap();
    assert!(out.text.contains("16000"));
}
```

Place this test in `stt/whisper.rs` under `#[cfg(test)]` with `FakeWhisperInference` defined in the test module — compile it **without** `feature = "whisper"` by moving the trait to `kea-infer` (no whisper-rs) and keeping the engine struct testable via conditional compilation of only the real `WhisperRsInference` impl behind `feature = "whisper"`.

**Resolution:** `WhisperInference` trait always in `kea-infer`. `WhisperSttEngine` always in `kea-engines` but calls `Arc<dyn WhisperInference>`. Real `WhisperRsInference` (uses `whisper-rs`) compiled only with `--features whisper`. Default tests inject `FakeWhisperInference`.

- [ ] **Step 1–5:** implement trait split, engine, wire into `register_phase2_stt_engines` under `#[cfg(feature = "whisper")]`, commit.

```bash
git commit --no-verify -m "feat(engines): WhisperSttEngine behind whisper feature with mock inference tests"
```

---

### Task 20: `DictationFeature` declaration

**Files:**
- Create: `crates/features/src/dictation.rs`
- Modify: `crates/features/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct DictationFeature;

  impl Feature for DictationFeature {
      fn id(&self) -> &str { "dictation" }
      fn required_caps(&self) -> Vec<CapSlot> {
          vec![CapSlot { name: "stt", kind: CapKind::Stt }]
      }
      fn commands(&self) -> Vec<Command> {
          vec![Command {
              id: "push_to_talk".into(),
              title: "Push to Talk".into(),
              default_accelerator: Some(default_dictation_accelerator().into()),
          }]
      }
  }

  fn default_dictation_accelerator() -> &'static str {
      #[cfg(target_os = "macos")]
      { "Cmd+Shift+D" }
      #[cfg(not(target_os = "macos"))]
      { "CommandOrControl+Shift+D" }
  }
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn dictation_declares_stt_slot_and_push_to_talk() {
    let f = DictationFeature;
    assert_eq!(f.id(), "dictation");
    assert_eq!(f.required_caps()[0].name, "stt");
    assert_eq!(f.required_caps()[0].kind, CapKind::Stt);
    assert_eq!(f.commands()[0].id, "push_to_talk");
}
```

- [ ] **Step 2–5:** implement, export from `lib.rs`, commit.

```bash
git commit --no-verify -m "feat(features): DictationFeature with stt slot"
```

---

### Task 21: `run_dictation()` orchestration

**Files:**
- Modify: `crates/features/src/dictation.rs`
- Modify: `crates/features/Cargo.toml` (deps already include `kea-platform`)

**Interfaces:**
- Consumes: `SlotResolver::resolve_stt`, `EngineRegistry::stt`, `BindingRepo`, `ActionRepo`, `AudioIo`, `TextIo`, `AudioPcm`/`SttOpts`/`Transcript`.
- Produces:
  ```rust
  pub struct DictationSession {
      pub audio: Box<dyn AudioIo>,
  }

  pub async fn run_dictation(
      engines: &EngineRegistry,
      bindings: &BindingRepo,
      actions: &ActionRepo,
      audio: &mut dyn AudioIo,
      textio: &dyn TextIo,
      pcm: PcmFrame,
      settings: &DictationSettings,
  ) -> Result<String, String>;
  ```

Flow:
1. `resolve_stt("dictation", "stt")` → engine id; load binding for model/provider_ref.
2. Convert `PcmFrame` → `AudioPcm` (resample to 16 kHz if needed via `kea_platform::audio::util::resample_linear`).
3. `actions.record(NewAction { feature_id: "dictation", command: "push_to_talk", ... })`.
4. `engine.transcribe(audio_pcm, SttOpts { model, provider_ref, .. }).await`.
5. Optional post-process (Task 22): if `settings.post_process`, call rewrite LLM with `RewriteMode::AudioRefinement` — **skip in this task**.
6. `textio.insert_at_cursor(&transcript.text).await`.
7. `actions.finish(id, "ok", None)`.

- [ ] **Step 1: Write the failing test with fakes**

```rust
struct FakeAudioIo { /* returns canned PcmFrame on stop */ }
struct FakeStt { /* registered as custom engine */ }

#[tokio::test]
async fn run_dictation_transcribes_and_inserts() {
    let mut reg = EngineRegistry::default();
    reg.register_stt(Arc::new(FakeStt { text: "hello world".into() }));

    let textio = Arc::new(FakeTextIo { /* records insert */ });
    let config_pool = open_pool("sqlite::memory:").await.unwrap();
    run_config_migrations(&config_pool).await.unwrap();
    let data_pool = open_pool("sqlite::memory:").await.unwrap();
    run_data_migrations(&data_pool).await.unwrap();
    let bindings = BindingRepo::new(config_pool);
    let actions = ActionRepo::new(data_pool);
    let settings = DictationSettings { post_process: false, active_model: None };

    let pcm = PcmFrame { samples: vec![0.0; 1600], sample_rate_hz: 16_000 };
    let out = run_dictation(
        &reg, &bindings, &actions,
        &mut FakeAudioIo::idle(),
        textio.as_ref(),
        pcm,
        &settings,
    ).await.unwrap();

    assert_eq!(out, "hello world");
    assert_eq!(textio.inserted.lock().unwrap().as_deref(), Some("hello world"));
    let rows = actions.recent(1).await.unwrap();
    assert_eq!(rows[0].feature_id, "dictation");
    assert_eq!(rows[0].status, "ok");
}
```

- [ ] **Step 2–5:** implement, PASS, commit.

```bash
git commit --no-verify -m "feat(features): run_dictation orchestration with action logging"
```

---

### Task 22: Optional `audio_refinement` post-process

**Files:**
- Modify: `crates/features/src/dictation.rs`

**Interfaces:**
- Consumes: `SlotResolver::resolve_llm`, `build_llm_request` with `RewriteMode::AudioRefinement`, existing rewrite path.
- Produces: when `DictationSettings.post_process == true`, after STT, run LLM refine on transcript text before insert.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn post_process_calls_llm_audio_refinement() {
    let mut reg = EngineRegistry::default();
    reg.register_stt(Arc::new(FakeStt { text: "um hello".into() }));
    reg.register_llm(Arc::new(NoopLlmEngine)); // echoes prompt
    let settings = DictationSettings { post_process: true, active_model: None };
    // ... assert output contains "echo:" and AudioRefinement prompt content
}
```

- [ ] **Step 2–5:** implement guarded block; commit.

```bash
git commit --no-verify -m "feat(features): optional audio_refinement post-process for dictation"
```

---

### Task 23: Tauri `AppState` — register STT engines + `DictationFeature`

**Files:**
- Modify: `src-tauri/src/main.rs`, `src-tauri/Cargo.toml` (`kea-infer`)

**Interfaces:**
- Extend `setup`:
  ```rust
  register_phase1_engines(&mut engines, ...);
  register_phase2_stt_engines(&mut engines, ...);
  features.register(Arc::new(DictationFeature));
  ```

- Store `ModelStorage` + `ModelDownloader` paths in `AppState` (or a new `InferState` struct):

  ```rust
  pub struct AppState {
      // existing fields...
      pub model_storage: kea_infer::ModelStorage,
      pub model_downloader: kea_infer::ModelDownloader,
      pub audio: Mutex<Box<dyn kea_platform::AudioIo>>,
  }
  ```

- [ ] **Step 1: Write failing test** in `commands.rs`:

```rust
#[test]
fn phase2_engine_ids_include_openai_stt() {
    let mut reg = EngineRegistry::default();
    register_phase2_stt_engines(&mut reg, ...);
    assert!(reg.list_stt_ids().contains(&"openai-stt".to_string()));
}
```

- [ ] **Step 2–5:** wire `AppState`, `new_audio_io()`, commit.

```bash
git commit --no-verify -m "feat(app): AppState with phase2 STT engines and audio I/O"
```

---

### Task 24: Tauri commands — STT engines, bindings, dictation, model download

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/main.rs`

**Interfaces:**
- Produces commands:
  ```rust
  #[tauri::command] async fn list_stt_engines(state) -> Vec<EngineInfoDto>
  #[tauri::command] async fn list_whisper_models(state) -> Vec<WhisperModelEntry>
  #[tauri::command] async fn list_installed_whisper_models(state) -> Vec<String>
  #[tauri::command] async fn download_whisper_model(state, model_id: String, app: AppHandle) -> Result<(), String>
  #[tauri::command] async fn get_dictation_settings(state) -> DictationSettings
  #[tauri::command] async fn set_dictation_settings(state, settings: DictationSettings) -> Result<(), String>
  #[tauri::command] async fn start_dictation(state, app: AppHandle) -> Result<(), String>
  #[tauri::command] async fn stop_dictation(state, app: AppHandle) -> Result<String, String>
  ```

- `list_stt_engines` mirrors `list_llm_engines` / `engine_infos` but calls `reg.list_stt_ids()` + `reg.stt(id).capabilities()`.
- `start_dictation`: `audio.lock().start_mic()`, emit `dictation:state` = `listening`.
- `stop_dictation`: `pcm = audio.stop_mic()`, spawn level polling task shutdown, call `run_dictation(...)`, emit `dictation:state` = `processing` then `idle`.
- `download_whisper_model`: runs `ModelDownloader::download_whisper` on `tauri::async_runtime::spawn`, emits `model:download:progress` events.

Constants (mirror rewrite pattern):

```rust
pub const DICTATION_FEATURE_ID: &str = "dictation";
pub const DICTATION_COMMAND_ID: &str = "push_to_talk";
pub const DICTATION_ACTION_ID: &str = "dictation:push_to_talk";
```

- [ ] **Step 1: Write failing test** for `stt_engine_infos` pure helper.

- [ ] **Step 2–5:** implement commands, register in `generate_handler!`, commit.

```bash
git commit --no-verify -m "feat(app): Tauri commands for STT, dictation, and model download"
```

---

### Task 25: Tauri events — `dictation:state`, `dictation:level`, `model:download:progress`

**Files:**
- Modify: `src-tauri/src/events.rs`, `src-tauri/src/main.rs`

**Interfaces:**
- Produces:
  ```rust
  #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
  pub struct DictationStatePayload { pub state: String } // "idle" | "listening" | "processing"

  #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
  pub struct DictationLevelPayload { pub level: f32 }    // 0.0..1.0

  #[derive(Clone, Debug, Serialize, PartialEq, Eq)]
  pub struct ModelDownloadProgressPayload {
      pub model_id: String,
      pub bytes_received: u64,
      pub bytes_total: u64,
  }

  pub fn emit_dictation_state(app: &AppHandle, state: &str);
  pub fn emit_dictation_level(app: &AppHandle, level: f32);
  pub fn emit_model_download_progress(app: &AppHandle, progress: &DownloadProgress);
  ```

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn dictation_level_payload_serializes() {
    let json = serde_json::to_string(&DictationLevelPayload { level: 0.5 }).unwrap();
    assert_eq!(json, r#"{"level":0.5}"#);
}
```

- [ ] **Step 3: Spawn level-meter task in `main.rs` during `start_dictation`**

```rust
// While listening, every 50ms:
let level = audio.current_level();
emit_dictation_level(&app, level);
```

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(app): dictation and model download Tauri events"
```

---

### Task 26: Push-to-talk hotkey dispatcher

**Files:**
- Modify: `src-tauri/src/main.rs`, `src-tauri/src/commands.rs`

**Interfaces:**
- Produces: hold-to-talk or toggle — **Phase 2 uses press-and-hold**: hotkey `Pressed` → `start_dictation`, `Released` → `stop_dictation`.

> If `global-hotkey` only exposes press events in the current version, use **toggle on each press** (press once start, press again stop) and document in Features UI. Prefer hold when the API supports `HotKeyState`.

- [ ] **Step 1: Register dictation hotkey** alongside rewrite in `setup`:

```rust
register_dictation_hotkey(&state, &app.handle()).await?;
```

- [ ] **Step 2: Extend hotkey dispatcher loop**

```rust
if action == DICTATION_ACTION_ID {
    emit_dictation_state(&app_handle, "listening");
    if let Err(e) = start_dictation_cmd(/* ... */).await {
        emit_dictation_error(&app_handle, &e);
    }
}
// On release / second press:
if action == DICTATION_ACTION_ID {
    match stop_dictation_cmd(/* ... */).await {
        Ok(text) => emit_dictation_progress(&app_handle, &format!("Inserted: {text}")),
        Err(e) => emit_dictation_error(&app_handle, &e),
    }
}
```

- [ ] **Step 3: Commit**

```bash
git commit --no-verify -m "feat(app): push-to-talk hotkey dispatcher for dictation"
```

---

### Task 27: UI `api.ts` — STT, dictation, model download

**Files:**
- Modify: `ui/src/api.ts`

**Interfaces:**
- Produces typed wrappers mirroring Task 24 commands + event listeners:

```ts
export type DictationState = "idle" | "listening" | "processing";
export type DictationSettings = { post_process: boolean; active_model: string | null };
export type WhisperModel = { id: string; display_name: string; url: string; size_bytes: number };
export type ModelDownloadProgress = { model_id: string; bytes_received: number; bytes_total: number };

export const listSttEngines = () => invoke<EngineInfo[]>("list_stt_engines");
export const listWhisperModels = () => invoke<WhisperModel[]>("list_whisper_models");
export const listInstalledWhisperModels = () => invoke<string[]>("list_installed_whisper_models");
export const downloadWhisperModel = (modelId: string) =>
  invoke<void>("download_whisper_model", { modelId });
export const getDictationSettings = () => invoke<DictationSettings>("get_dictation_settings");
export const setDictationSettings = (settings: DictationSettings) =>
  invoke<void>("set_dictation_settings", { settings });
export const startDictation = () => invoke<void>("start_dictation");
export const stopDictation = () => invoke<string>("stop_dictation");

export const onDictationState = (cb: (state: DictationState) => void) =>
  listen<{ state: DictationState }>("dictation:state", (e) => cb(e.payload.state));
export const onDictationLevel = (cb: (level: number) => void) =>
  listen<{ level: number }>("dictation:level", (e) => cb(e.payload.level));
export const onModelDownloadProgress = (cb: (p: ModelDownloadProgress) => void) =>
  listen<ModelDownloadProgress>("model:download:progress", (e) => cb(e.payload));
```

- [ ] **Step 1:** `npm run typecheck` — FAIL until commands exist.
- [ ] **Step 2–5:** add exports, PASS, commit.

```bash
git commit --no-verify -m "feat(ui): typed Phase 2 dictation and model API"
```

---

### Task 28: UI `LevelMeter` component

**Files:**
- Create: `ui/src/components/LevelMeter.tsx`

**Interfaces:**
- Props: `{ level: number }` — horizontal bar, width = `level * 100%`, green fill.

- [ ] **Step 1: Render test via typecheck** — import in `SpeechOverlay.tsx`.
- [ ] **Step 2–5:** implement, commit.

```bash
git commit --no-verify -m "feat(ui): LevelMeter component"
```

---

### Task 29: UI `SpeechOverlay` + wire events in `App.tsx`

**Files:**
- Create: `ui/src/components/SpeechOverlay.tsx`
- Modify: `ui/src/App.tsx`

**Interfaces:**
- Consumes: `onDictationState`, `onDictationLevel`.
- Produces: fixed overlay (top-right) with state pill (`Idle` / `Listening…` / `Processing…`) + `LevelMeter` visible only when `listening`.

```tsx
export default function SpeechOverlay() {
  const [state, setState] = useState<DictationState>("idle");
  const [level, setLevel] = useState(0);
  useEffect(() => {
    const unsubs = Promise.all([
      onDictationState(setState),
      onDictationLevel(setLevel),
    ]);
    return () => { void unsubs.then((fns) => fns.forEach((fn) => fn())); };
  }, []);
  // render StatusPill-style state + LevelMeter when state === "listening"
}
```

- [ ] **Step 1–5:** implement, mount `<SpeechOverlay />` in `App.tsx` beside existing `StatusPill`, commit.

```bash
git commit --no-verify -m "feat(ui): SpeechOverlay with dictation state and level meter"
```

---

### Task 30: UI `DictationPanel` on Features page

**Files:**
- Create: `ui/src/components/DictationPanel.tsx`
- Modify: `ui/src/pages/FeaturesPage.tsx`

**Interfaces:**
- Composes: `SlotBinder` (feature `dictation`, slot `stt`), `HotkeyBinder` (`push_to_talk`), `SettingsForm` toggle for `post_process`, manual Start/Stop buttons calling `startDictation`/`stopDictation`.

- [ ] **Step 1:** typecheck fails without component.
- [ ] **Step 2–5:** implement panel section below rewrite panel, commit.

```bash
git commit --no-verify -m "feat(ui): DictationPanel on Features page"
```

---

### Task 31: UI `ModelManager` on Configuration page

**Files:**
- Create: `ui/src/components/ModelManager.tsx`
- Modify: `ui/src/pages/ConfigurationPage.tsx`

**Interfaces:**
- Consumes: `listWhisperModels`, `listInstalledWhisperModels`, `downloadWhisperModel`, `onModelDownloadProgress`, `getDictationSettings`, `setDictationSettings`.
- Produces: table of Whisper models with size, Install button, progress bar, "Active model" selector (writes `dictation.active_model`).

- [ ] **Step 1–5:** implement, wire page, commit.

```bash
git commit --no-verify -m "feat(ui): ModelManager for Whisper download on Configuration page"
```

---

### Task 32: End-to-end acceptance (macOS manual + CI compile)

**Files:** none (verification only)

- [ ] **Step 1: CI (default features — no whisper.cpp)**

Run: `cargo test --workspace && cargo build -p kea-app && (cd ui && npm run build)`
Expected: PASS on macOS, Windows, Linux matrix jobs.

- [ ] **Step 2: Optional whisper feature build (manual / separate CI job)**

Run: `cargo build -p kea-app --features whisper`
Expected: compiles with `whisper-rs` linked; **not** required for default CI green.

- [ ] **Step 3: macOS manual checklist**

1. `cargo tauri dev`
2. Grant Microphone permission when prompted (System Settings > Privacy & Security).
3. Configuration → set OpenAI provider + API key; ModelManager → download `ggml-base.en` (or use `openai-stt` remote).
4. Features → bind dictation `stt` slot to `openai-stt` (or `whisper` if built with feature); set push-to-talk hotkey `Cmd+Shift+D`.
5. Place caret in TextEdit → hold hotkey → speak → release → transcript inserted at cursor; `data.db` `actions` row with `feature_id=dictation`, `status=ok`.
6. Speech overlay shows level meter while listening.
7. Enable post-process → verify `audio_refinement` LLM pass runs (requires bound rewrite `llm` slot).

- [ ] **Step 4: Document deferred platform checks**

Windows/Linux mic capture manual checks land when Tasks 13–14 complete.

---

## Phase 2 Definition of Done

- `cargo test --workspace` green **without** `--features whisper`; `cargo build -p kea-app` succeeds; `ui` builds on CI (macOS, Windows, Linux).
- **macOS:** push-to-talk hotkey → mic capture → `SttEngine::transcribe` → insert at cursor works with remote OpenAI STT; optional local Whisper works when app built with `--features whisper` and model downloaded via ModelManager.
- `SttEngine` trait extended with `transcribe(AudioPcm, SttOpts) -> Result<Transcript, EngineError>`; `AudioPcm`, `SttOpts`, `Transcript` defined in `kea-engines::traits`.
- `openai-stt` engine registered via `register_phase2_stt_engines`; HTTP tests use `wiremock` — no CI network calls.
- `whisper` engine behind cargo feature; default CI does **not** build whisper.cpp; unit tests use `WhisperInference` mock.
- `SlotResolver::resolve_stt` mirrors `resolve_llm`; `DictationFeature` declares `stt` slot + `push_to_talk` command.
- `platform/audio` `AudioIo` trait + macOS `cpal` impl; pure resample/RMS helpers unit-tested.
- `TextIo::insert_at_cursor` reuses D4 clipboard+paste path.
- `kea-infer`: model registry, `ModelStorage`, `ModelDownloader` with injectable transport; progress events to UI.
- Tauri commands expose STT listing, dictation start/stop, model download; events `dictation:state`, `dictation:level`, `model:download:progress`.
- UI: `SpeechOverlay` (level + state), `DictationPanel` on Features, `ModelManager` on Configuration.
- Unit tests use fakes for mic, HTTP, and Whisper inference — no real mic, network, or GGUF model in `cargo test`.

## Self-Review (spec coverage map)

| Spec reference | Plan tasks |
|----------------|------------|
| §3 D1 plugin model | Tasks 3, 8, 20, 23 (registries + dictation feature) |
| §3 D5 offline Whisper via whisper-rs | Tasks 16–19, 31 (infer + feature-gated engine) |
| §3 D9 config.db / keyring boundary | Tasks 8–9, 24 (settings repo; credentials via existing seam) |
| §3 D10 React UI | Tasks 27–31 |
| §4.1 `SttEngine::transcribe` | Tasks 2–3, 7–8, 19 |
| §4.2 `AudioIo` mic capture | Tasks 10–14 (`capture_system`/`play` deferred) |
| §4.3 Dictation feature plugin | Tasks 20–22 |
| §4.4 slot resolution | Tasks 4, 21 (`resolve_stt`) |
| §5 Dictation data flow (4 steps) | Tasks 12, 15, 21, 25–26 |
| §7 `LevelMeter`, `StatusPill`, `ModelManager` | Tasks 28–31 |
| §8 integration matrix mic row (`cpal`) | Tasks 12–14 |
| §9 Phase 2 outcome | Definition of Done |
| §11 testing strategy (mocked engines, no streaming) | Global Constraints; Tasks 7, 18–19, 21 |

### How tests avoid real I/O

| Risk | Mitigation |
|------|------------|
| Real microphone | `FakeAudioIo` in Tasks 10, 21; `MacAudioIo` tests only cover state machine; manual macOS checklist for real mic |
| Real HTTP / OpenAI | `wiremock` in Tasks 5, 7; `FakeTransport` in Task 18 |
| Building whisper.cpp | `whisper` cargo feature off by default; Task 19 tests inject `FakeWhisperInference`; real `WhisperRsInference` only with `--features whisper` |
| Real GGUF model on disk | Task 19 writes tiny fake file; no model download in unit tests |

### Deferred to later phases (explicit boundaries)

| Item | Phase | Notes |
|------|-------|-------|
| **Parakeet / sherpa-onnx STT (D6)** | Phase 4 | Not on dictation critical path; OpenAI + Whisper suffice for Phase 2 |
| **TTS engines + `AudioIo::play` (D11)** | Phase 4 | Playback trait method stubbed or omitted in Phase 2 |
| **Meetings feature, loopback/`capture_system` (D14)** | Phase 3 | System audio capture added to `platform/audio` in Phase 3 |
| **Streaming partial transcription** | Non-goal (§2) | Bounded buffer + single `transcribe` call per PTT session |
| **macOS Accessibility insertion enhancement (D12)** | Phase 4 | Dictation uses D4 clipboard+paste `insert_at_cursor` |
| **GPU flags (Metal/CUDA/Vulkan) for Whisper** | Phase 4 polish | Phase 2 ships CPU inference baseline; GPU via infer feature flags later |
| **History / Logs pages** | Phase 4 | Actions recorded in Phase 2; full History UI deferred |
| **Separate speech overlay Tauri window** | Optional polish | Phase 2 overlay lives in main webview; detached overlay window Phase 4 |
| **Windows/Linux AudioIo** | Parallel Tasks 13–14 | May trail macOS E2E; stubs must compile |

### Phase 2 vs Phase 4 scope decision (Whisper)

- **Done in Phase 2:** Whisper model catalog, download, on-disk storage, `ModelManager` UI, `WhisperSttEngine` behind `--features whisper`, CPU transcription path.
- **Deferred to Phase 4:** Parakeet engine, TTS, GPU acceleration matrix, notarized installers, NSServices, autostart wizards, Parakeet sharing ONNX runtime with TTS (D11).
