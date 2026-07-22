# KEA Phase 4 — Parity Polish, Later Engines, Distribution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close functional parity gaps deferred from Phases 0–3: extend the stub `TtsEngine` and ship OpenAI + local (sherpa-onnx) TTS with `AudioIo::play`; add Parakeet STT via sherpa-onnx (D6, `ort` fallback documented); wire read-aloud (`TtsFeature` + `run_tts`); macOS Accessibility insertion enhancement (D12); History/Activity + Logs UI pages (§7.2, §6.4); autostart, notifications, and first-run permission surfaces; Tauri/UI wiring for TTS engine selection and ONNX model management (reuse `ModelManager` / `kea-infer`). **Part B** captures distribution, signing, GPU CI matrices, Swift retirement, and the real per-OS manual parity matrix — work a human must finish outside automated agent runs.

**Architecture:** Phase 0–3 shipped `SttEngine::transcribe`, `LlmEngine::complete`, `AudioIo` mic + meeting capture, `TextIo` clipboard+paste (D4), `SlotResolver::resolve_stt`/`resolve_llm`, `ActionRepo`, `Permissions` (Mic + Screen Recording), and feature orchestration templates (`run_dictation`, `run_rewrite`, `run_meeting_*`). Phase 4 extends `EngineRegistry` with TTS, `SlotResolver::resolve_tts`, optional sherpa-onnx backends behind cargo features (mirroring `whisper`), `AudioIo::play` with a **default no-op** so existing fakes stay green, `TextIo::replace_with_mode` for Accessibility (D12), `core/log::tail_log_file`, enriched `ActionRepo` + optional `conversations` tables for History, and thin Tauri/React surfaces. Consumers depend on traits; `src-tauri` is the only composition root.

**Tech Stack:** Rust (edition 2021), Tauri 2.x, `reqwest` + `wiremock` (HTTP TTS tests), `rodio` + `cpal` (playback), `sherpa-onnx` (optional — Parakeet STT + local TTS, D6/D11), `sqlx`, `tokio`, `async-trait`, `tracing`/`tracing-subscriber`, Vite + React + TypeScript (D10), `tauri-plugin-autostart`, `tauri-plugin-notification` (Part B: `tauri-plugin-updater`).

## Global Constraints

- **Product name:** `KEA` everywhere (`kea-*` crates, `ai.kea.app`). _(D13.)_
- **Plugin model:** internal trait + registry, compiled in. No dynamic loading. _(D1, D2.)_
- **Storage boundary (D9):** `config.db` = settings, bindings, hotkeys, provider config, TTS/Parakeet model install metadata; `data.db` = actions, conversations (if present), meeting content; **keyring = credentials only**. DB rows store `engine_id`, `model`, `provider_ref` — never secrets.
- **Web UI (D10):** React pages composed from the shared component library; Rust plugins expose typed Tauri commands/events only.
- **Async:** all engine/platform I/O trait methods are `async` (`async-trait`).
- **TDD:** every Part A code task is test-first. Rust async tests use `#[tokio::test]`. Store tests use `sqlite::memory:`; HTTP engines use `wiremock` or injected `HttpClient`; sherpa engines use injectable `SherpaSttInference` / `SherpaTtsInference` trait fakes — **never hit real OpenAI, real audio output, or real ONNX models in unit tests**.
- **No real audio in unit tests:** `AudioIo::play` is exercised via `FakeAudioIo` recording PCM; macOS `rodio`/`cpal` playback is compile + manual acceptance only.
- **No real network in unit tests:** OpenAI TTS uses `wiremock`; model download tests mock `DownloadTransport`.
- **Feature-gate all heavy native deps OFF by default:**
  - `whisper` (existing) — unchanged; default CI does not build whisper.cpp.
  - `sherpa` on `kea-infer` — enables sherpa-onnx Rust bindings + shared ONNX runtime plumbing.
  - `parakeet` on `kea-engines` — `["kea-infer/sherpa"]`; registers `ParakeetSttEngine`.
  - `tts-local` on `kea-engines` — `["kea-infer/sherpa"]`; registers `SherpaTtsEngine`.
  - `system-audio-sck` (Phase 3) — unchanged; still off in default CI.
  - Default `cargo test --workspace` and CI **must stay green without** `--features whisper,sherpa,parakeet,tts-local,system-audio-sck`.
- **macOS-first:** Tasks through macOS TTS read-aloud E2E must pass before treating Phase 4 buildable work done on the primary dev machine. Windows/Linux playback and Accessibility parallels are labeled **parallel per-OS**.
- **Sherpa best-effort:** Like Whisper/SCK, sherpa-onnx integration is additive. Parakeet/local-TTS unit tests run against trait fakes; real sherpa-onnx compile is manual/`--features sherpa,parakeet,tts-local` only.
- **`ort` fallback (D6):** If sherpa-onnx Parakeet bindings block release, document switching to raw `ort` session inference in `docs/cross-platform/plans/CONTRACTS.md` — not implemented in default path; spike result recorded in Task 9 notes.
- **Targets:** code compiles on macOS, Windows, Linux; CI runs `cargo test --workspace` (default features) on all three.
- **Commits:** frequent conventional commits, one per task minimum. Use `git commit --no-verify` when the legacy Vox FluidAudio pre-commit hook blocks unrelated paths.

### Sherpa / ONNX feature-gate decision

| Feature flag | Crate | Enables | Default CI |
|--------------|-------|---------|------------|
| `sherpa` | `kea-infer` | `sherpa-onnx` dep, `SherpaSttInference` / `SherpaTtsInference` trait + real impl module | **OFF** |
| `parakeet` | `kea-engines` | `ParakeetSttEngine` registration | **OFF** |
| `tts-local` | `kea-engines` | `SherpaTtsEngine` registration | **OFF** |
| `whisper` | `kea-engines`, `kea-infer` | existing Whisper path | **OFF** |

One ONNX runtime stack (`sherpa-onnx`) powers **both** Parakeet STT (D6) and local TTS (D11) when features are enabled at build time.

---

## File Structure

```
kea/
├─ Cargo.toml                              # rodio workspace dep; optional sherpa-onnx
├─ crates/
│  ├─ core/
│  │  ├─ migrations/
│  │  │  ├─ config/0005_tts.sql            # TTS settings KV
│  │  │  └─ data/0003_conversations.sql    # conversations + messages (History)
│  │  └─ src/
│  │     ├─ resolve.rs                     # + resolve_tts
│  │     ├─ log.rs                         # + tail_log_file, log_path helpers
│  │     ├─ tts/
│  │     │  ├─ mod.rs
│  │     │  └─ settings.rs                 # TtsSettings repo (config.db)
│  │     └─ store/
│  │        ├─ actions.rs                  # + search, detail, prune
│  │        └─ conversations.rs            # ConversationRepo (data.db)
│  ├─ engines/
│  │  ├─ Cargo.toml                        # features: parakeet, tts-local
│  │  └─ src/
│  │     ├─ traits.rs                       # TtsOpts, TtsEngine::synthesize
│  │     ├─ registry.rs                     # register_tts, list_tts_ids, tts()
│  │     ├─ http.rs                         # + post_binary (TTS audio bytes)
│  │     ├─ noop.rs                         # NoopTtsEngine
│  │     ├─ tts/
│  │     │  ├─ mod.rs
│  │     │  ├─ openai.rs                    # OpenAiTtsEngine (wiremock-tested)
│  │     │  └─ sherpa.rs                    # [feature tts-local] SherpaTtsEngine
│  │     ├─ stt/
│  │     │  └─ parakeet.rs                  # [feature parakeet] ParakeetSttEngine
│  │     └─ lib.rs                          # register_phase4_* helpers
│  ├─ infer/
│  │  ├─ Cargo.toml                         # feature sherpa = [dep:sherpa-onnx]
│  │  └─ src/
│  │     ├─ registry.rs                     # + parakeet_catalog, tts_catalog
│  │     ├─ sherpa_stt.rs                   # SherpaSttInference trait + [feature] impl
│  │     └─ sherpa_tts.rs                   # SherpaTtsInference trait + [feature] impl
│  ├─ features/
│  │  └─ src/
│  │     ├─ tts.rs                          # TtsFeature + run_tts()
│  │     └─ lib.rs
│  └─ platform/
│     └─ src/
│        ├─ audio/
│        │  ├─ mod.rs                       # AudioIo::play default + trait method
│        │  ├─ playback.rs                  # rodio/cpal macOS impl
│        │  └─ stub.rs                      # stub play (non-macOS compile)
│        ├─ permissions/
│        │  └─ mod.rs                       # + PermKind::Accessibility
│        └─ textio/
│           ├─ mod.rs                       # ReplaceMode, replace_with_mode
│           └─ macos_ax.rs                  # [macOS] Accessibility insertion (D12)
├─ src-tauri/src/
│  ├─ main.rs                                # TtsFeature, autostart/notification plugins
│  ├─ commands.rs                            # history, logs, tts, onnx model commands
│  └─ events.rs                              # tts:state events
└─ ui/src/
   ├─ api.ts                                 # history, logs, tts wrappers
   ├─ App.tsx                                 # + History, Logs, Settings nav
   ├─ components/
   │  ├─ HistoryPanel.tsx
   │  ├─ LogsViewer.tsx
   │  ├─ TtsPanel.tsx
   │  └─ ModelManager.tsx                    # extend for parakeet + tts model kinds
   └─ pages/
      ├─ HistoryPage.tsx
      ├─ LogsPage.tsx
      └─ SettingsPage.tsx                    # autostart, permissions, log level
```

---

# PART A — BUILDABLE ENGINEERING

Bite-sized TDD tasks. Each ends with `git commit --no-verify`.

---

### Task 1: Workspace + crate dependencies for Phase 4

**Files:**
- Modify: `Cargo.toml`, `crates/infer/Cargo.toml`, `crates/engines/Cargo.toml`, `crates/platform/Cargo.toml`, `src-tauri/Cargo.toml`

**Interfaces:**
- Workspace dep `rodio = "0.19"`.
- `kea-infer` feature `sherpa = ["dep:sherpa-onnx"]`; optional `sherpa-onnx` crate (pin version after spike).
- `kea-engines` features: `parakeet = ["kea-infer/sherpa"]`, `tts-local = ["kea-infer/sherpa"]`; default `[]`.
- `kea-platform`: `rodio.workspace = true`.
- `kea-app` features: `parakeet = ["kea-engines/parakeet"]`, `tts-local = ["kea-engines/tts-local"]`, `sherpa = ["kea-infer/sherpa"]`.

- [ ] **Step 1: Write the failing test**

`crates/platform/src/lib.rs`:

```rust
#[cfg(test)]
mod phase4_dep_tests {
    #[test]
    fn rodio_is_linked() {
        let _ = std::any::type_name::<rodio::OutputStream>();
    }
}
```

- [ ] **Step 2: Run test — FAIL**

Run: `cargo test -p kea-platform rodio_is_linked`

- [ ] **Step 3: Add dependencies (sherpa-onnx optional, not in default build)**

Root `Cargo.toml`:

```toml
rodio = "0.19"
```

`crates/infer/Cargo.toml`:

```toml
[features]
default = []
whisper = ["dep:whisper-rs"]
sherpa = ["dep:sherpa-onnx"]

[dependencies.sherpa-onnx]
version = "0.5"
optional = true
```

`crates/engines/Cargo.toml`:

```toml
[features]
default = []
whisper = ["kea-infer/whisper", "dep:whisper-rs"]
parakeet = ["kea-infer/sherpa"]
tts-local = ["kea-infer/sherpa"]
```

- [ ] **Step 4: Run test — PASS**

Run: `cargo test -p kea-platform rodio_is_linked`

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(phase4): add rodio and optional sherpa-onnx deps"
```

---

### Task 2: Extend `TtsEngine` trait — `TtsOpts`, `synthesize`

**Files:**
- Modify: `crates/engines/src/traits.rs`

**Interfaces:**
- Current stub:

```rust
#[async_trait]
pub trait TtsEngine: Send + Sync { fn id(&self) -> &str; fn capabilities(&self) -> EngineCaps; }
```

- Target:

```rust
#[derive(Debug, Clone, Default)]
pub struct TtsOpts {
    pub model: Option<String>,
    pub voice: Option<String>,
    pub provider_ref: Option<String>,
}

#[async_trait]
pub trait TtsEngine: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EngineCaps;
    async fn synthesize(&self, text: &str, opts: TtsOpts) -> Result<AudioPcm, EngineError>;
}
```

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tts_types_tests {
    use super::*;

    struct EchoTts;
    #[async_trait]
    impl TtsEngine for EchoTts {
        fn id(&self) -> &str { "echo-tts" }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec!["echo".into()] }
        }
        async fn synthesize(&self, text: &str, _opts: TtsOpts) -> Result<AudioPcm, EngineError> {
            let n = text.len().min(100);
            Ok(AudioPcm {
                samples: vec![0.1; n * 100],
                sample_rate_hz: 24_000,
            })
        }
    }

    #[tokio::test]
    async fn tts_engine_synthesize_returns_pcm() {
        let engine = EchoTts;
        let pcm = engine.synthesize("hello", TtsOpts::default()).await.unwrap();
        assert_eq!(pcm.sample_rate_hz, 24_000);
        assert!(!pcm.samples.is_empty());
    }
}
```

- [ ] **Step 2: Run test — FAIL** (`synthesize` not in trait)

Run: `cargo test -p kea-engines tts_engine_synthesize`

- [ ] **Step 3: Implement `TtsOpts` + `synthesize` on trait**

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(engines): extend TtsEngine with synthesize"
```

---

### Task 3: `EngineRegistry` TTS slots + `NoopTtsEngine`

**Files:**
- Modify: `crates/engines/src/registry.rs`, `crates/engines/src/noop.rs`, `crates/engines/src/lib.rs`

**Interfaces:**

```rust
// registry.rs
pub fn register_tts(&mut self, e: Arc<dyn TtsEngine>);
pub fn tts(&self, id: &str) -> Option<Arc<dyn TtsEngine>>;
pub fn list_tts_ids(&self) -> Vec<String>;
```

```rust
// noop.rs
pub struct NoopTtsEngine;
// id = "noop-tts"; synthesize returns 24000 Hz silence sized by text.len()
```

- [ ] **Step 1: Write failing registry test**

```rust
#[tokio::test]
async fn register_and_synthesize_noop_tts() {
    let mut reg = EngineRegistry::default();
    reg.register_tts(Arc::new(NoopTtsEngine));
    assert_eq!(reg.list_tts_ids(), vec!["noop-tts".to_string()]);
    let pcm = reg.tts("noop-tts").unwrap()
        .synthesize("hi", TtsOpts::default()).await.unwrap();
    assert_eq!(pcm.sample_rate_hz, 24_000);
}
```

- [ ] **Step 2–4: Implement, PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(engines): TTS registry and NoopTtsEngine"
```

---

### Task 4: `SlotResolver::resolve_tts`

**Files:**
- Modify: `crates/core/src/resolve.rs`

**Interfaces:**

```rust
pub async fn resolve_tts(&self, feature_id: &str, slot: &str) -> Result<Resolution, KeaError> {
    if let Some(b) = self.bindings.get(feature_id, slot).await? {
        return Ok(if self.engines.tts(&b.engine_id).is_some() {
            Resolution::Bound(b.engine_id)
        } else {
            Resolution::Unresolvable
        });
    }
    let candidates = self.engines.list_tts_ids();
    Ok(match candidates.len() {
        0 => Resolution::Unresolvable,
        1 => Resolution::Bound(candidates.into_iter().next().unwrap()),
        _ => Resolution::NeedsChoice(candidates),
    })
}
```

- [ ] **Step 1: Mirror `resolve_stt` tests with `NoopTtsEngine` + `SecondNoopTts`**

```rust
#[tokio::test]
async fn unbound_single_tts_engine_autobinds() {
    let (bindings, mut reg) = setup().await;
    reg.register_tts(Arc::new(NoopTtsEngine));
    let r = SlotResolver::new(&reg, &bindings)
        .resolve_tts("tts", "tts").await.unwrap();
    assert!(matches!(r, Resolution::Bound(id) if id == "noop-tts"));
}
```

- [ ] **Step 2–5: Implement, PASS, commit**

```bash
git commit --no-verify -m "feat(core): resolve_tts slot resolution"
```

---

### Task 5: `HttpClient::post_binary` for TTS audio responses

**Files:**
- Modify: `crates/engines/src/http.rs`

**Interfaces:**

```rust
#[async_trait]
pub trait HttpClient: Send + Sync {
    // existing post_json, post_multipart ...
    async fn post_json_tts(
        &self,
        url: &str,
        bearer: &str,
        body: serde_json::Value,
    ) -> Result<(u16, Vec<u8>), EngineError>;
}
```

(`post_json_tts` returns raw bytes — OpenAI `/audio/speech` responds with `audio/mpeg` or `audio/wav`.)

- [ ] **Step 1: Failing test with `wiremock` returning `Content-Type: audio/mpeg` body**

```rust
#[tokio::test]
async fn post_json_tts_returns_binary_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(ResponseTemplate::new(200)
            .set_body_bytes(b"FAKEAUDIO"))
        .mount(&server).await;
    let client = ReqwestHttpClient::new();
    let (status, bytes) = client
        .post_json_tts(&format!("{}/v1/audio/speech", server.uri()), "sk-test",
            serde_json::json!({"model":"tts-1","input":"hi","voice":"alloy"}))
        .await.unwrap();
    assert_eq!(status, 200);
    assert_eq!(bytes, b"FAKEAUDIO");
}
```

- [ ] **Step 2–5: Implement on `ReqwestHttpClient`, PASS, commit**

```bash
git commit --no-verify -m "feat(engines): HttpClient post_json_tts for binary TTS"
```

---

### Task 6: `OpenAiTtsEngine` (HTTP via `HttpClient` + provider seam)

**Files:**
- Create: `crates/engines/src/tts/mod.rs`, `crates/engines/src/tts/openai.rs`
- Modify: `crates/engines/src/lib.rs`

**Interfaces:**

```rust
pub struct OpenAiTtsEngine {
    pub http: Arc<dyn HttpClient>,
    pub credentials: Arc<dyn CredentialSource>,
    pub configs: Arc<dyn ProviderConfigSource>,
    pub provider_ref: String,
}

// id = "openai-tts"
// capabilities.models = ["tts-1", "tts-1-hd", "gpt-4o-mini-tts"]
// synthesize: POST {base}/audio/speech → decode wav/mp3 bytes → AudioPcm
```

Add `crates/engines/src/tts/audio.rs` with `bytes_to_pcm_wav(&[u8]) -> Result<AudioPcm, EngineError>` (parse RIFF header; reject mp3 in unit tests or use fixture wav).

- [ ] **Step 1: wiremock test — 200 + minimal WAV fixture → `AudioPcm`**

```rust
#[tokio::test]
async fn openai_tts_synthesizes_wav() {
  // Mock /v1/audio/speech; inject FakeCredentials + FakeConfigs
  // assert pcm.sample_rate_hz > 0
}
```

- [ ] **Step 2–5: Implement, PASS, commit**

```bash
git commit --no-verify -m "feat(engines): OpenAI TTS engine with wiremock tests"
```

---

### Task 7: `register_phase4_tts_engines` helper

**Files:**
- Modify: `crates/engines/src/lib.rs`

**Interfaces:**

```rust
pub fn register_phase4_tts_engines(
    reg: &mut EngineRegistry,
    http: Arc<dyn HttpClient>,
    credentials: Arc<dyn CredentialSource>,
    configs: Arc<dyn ProviderConfigSource>,
) {
    reg.register_tts(Arc::new(OpenAiTtsEngine {
        http,
        credentials: credentials.clone(),
        configs: configs.clone(),
        provider_ref: "openai".into(),
    }));
}

#[cfg(feature = "tts-local")]
pub fn register_sherpa_tts_engine(reg: &mut EngineRegistry, inference: Arc<dyn SherpaTtsInference>) {
    reg.register_tts(Arc::new(SherpaTtsEngine { inference }));
}
```

- [ ] **Step 1: Test — default build lists only `openai-tts`**

```rust
#[test]
fn registers_openai_tts_engine() {
    let mut reg = EngineRegistry::default();
    register_phase4_tts_engines(&mut reg, ...);
    let ids = reg.list_tts_ids();
    assert!(ids.contains(&"openai-tts".to_string()));
    assert!(!ids.contains(&"sherpa-tts".to_string()));
}
```

- [ ] **Step 2–5: Wire, PASS, commit**

```bash
git commit --no-verify -m "feat(engines): register_phase4_tts_engines"
```

---

### Task 8: `kea-infer` — ONNX model catalog + `SherpaSttInference` / `SherpaTtsInference` traits

**Files:**
- Create: `crates/infer/src/sherpa_stt.rs`, `crates/infer/src/sherpa_tts.rs`
- Modify: `crates/infer/src/registry.rs`, `crates/infer/src/lib.rs`

**Interfaces:**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OnnxModelEntry {
    pub id: String,
    pub display_name: String,
    pub url: String,
    pub size_bytes: u64,
    pub sha256: String,
    pub kind: OnnxModelKind, // Parakeet | TtsVits
}

impl ModelRegistry {
    pub fn parakeet_catalog() -> Vec<OnnxModelEntry> { /* e.g. parakeet-tdt-0.6b */ }
    pub fn tts_catalog() -> Vec<OnnxModelEntry> { /* e.g. vits-piper-en */ }
}

#[async_trait]
pub trait SherpaSttInference: Send + Sync {
    async fn transcribe_parakeet(
        &self,
        pcm: AudioPcm,
        model_dir: &Path,
        opts: WhisperOpts, // reuse language opt
    ) -> Result<String, InferError>;
}

#[async_trait]
pub trait SherpaTtsInference: Send + Sync {
    async fn synthesize(
        &self,
        text: &str,
        model_dir: &Path,
    ) -> Result<AudioPcm, InferError>;
}
```

`#[cfg(feature = "sherpa")]` modules `SherpaOnnxSttInference` / `SherpaOnnxTtsInference` wrap the real crate; **default tests use fakes only**.

- [ ] **Step 1: Catalog tests (no sherpa dep)**

```rust
#[test]
fn parakeet_catalog_has_entry() {
    let models = ModelRegistry::parakeet_catalog();
    assert!(!models.is_empty());
    assert!(models[0].url.starts_with("https://"));
}
```

- [ ] **Step 2: Fake inference test**

```rust
struct FakeSherpaStt;
#[async_trait]
impl SherpaSttInference for FakeSherpaStt {
    async fn transcribe_parakeet(&self, pcm: AudioPcm, _dir: &Path, _opts: WhisperOpts) -> Result<String, InferError> {
        Ok(format!("parakeet: {} samples", pcm.samples.len()))
    }
}
```

- [ ] **Step 3–5: Implement catalogs + traits; `[feature sherpa]` real impl stub; commit**

```bash
git commit --no-verify -m "feat(infer): sherpa inference traits and ONNX model catalogs"
```

---

### Task 9: Parakeet STT engine — FEATURE-GATED (`parakeet`)

**Files:**
- Create: `crates/engines/src/stt/parakeet.rs`
- Modify: `crates/engines/src/lib.rs`, `crates/engines/Cargo.toml`

**Interfaces:**

```rust
pub struct ParakeetSttEngine {
    pub inference: Arc<dyn SherpaSttInference>,
    pub storage: Arc<ModelStorage>,
}

// id = "parakeet"
// capabilities.models = ModelRegistry::parakeet_catalog().map(|e| e.id)
// transcribe: resolve model path via storage → inference.transcribe_parakeet
```

```rust
#[cfg(feature = "parakeet")]
pub fn register_parakeet_stt_engine(
    reg: &mut EngineRegistry,
    inference: Arc<dyn SherpaSttInference>,
    storage: Arc<ModelStorage>,
) {
    reg.register_stt(Arc::new(ParakeetSttEngine { inference, storage }));
}
```

**D6 `ort` fallback note:** If Task 9 spike (`cargo build -p kea-engines --features parakeet`) fails on sherpa-onnx Parakeet API, add `docs/cross-platform/plans/CONTRACTS.md` section "Parakeet ort fallback" with export steps + `OrtParakeetInference` trait impl plan — do **not** block default CI.

- [ ] **Step 1: Unit test with `FakeSherpaStt` + temp model dir (no real ONNX)**

```rust
#[tokio::test]
async fn parakeet_stt_uses_injected_inference() {
    let engine = ParakeetSttEngine {
        inference: Arc::new(FakeSherpaStt),
        storage: Arc::new(ModelStorage::new(tempdir())),
    };
    let out = engine.transcribe(AudioPcm { samples: vec![0.0; 1600], sample_rate_hz: 16_000 }, SttOpts::default()).await.unwrap();
    assert!(out.text.contains("parakeet"));
}
```

- [ ] **Step 2–4: Implement engine module (compiled only with `feature = "parakeet"`); default `cargo test` skips module via `#[cfg(feature = "parakeet")]` on integration test or use always-compiled engine with injected fake (preferred — engine file always compiles, sherpa real impl behind feature).**

**Preferred pattern (matches Whisper):** `parakeet.rs` always compiles; depends on `SherpaSttInference` trait only; real `SherpaOnnxSttInference` behind `kea-infer/sherpa`.

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(engines): Parakeet STT engine behind parakeet feature"
```

---

### Task 10: Local TTS engine — FEATURE-GATED (`tts-local`)

**Files:**
- Create: `crates/engines/src/tts/sherpa.rs`
- Modify: `crates/engines/src/tts/mod.rs`, `crates/engines/src/lib.rs`

**Interfaces:**

```rust
pub struct SherpaTtsEngine {
    pub inference: Arc<dyn SherpaTtsInference>,
    pub storage: Arc<ModelStorage>,
}

// id = "sherpa-tts"
// synthesize: storage path → inference.synthesize(text, model_dir)
```

- [ ] **Step 1: Unit test with `FakeSherpaTts`**

```rust
struct FakeSherpaTts;
#[async_trait]
impl SherpaTtsInference for FakeSherpaTts {
    async fn synthesize(&self, text: &str, _dir: &Path) -> Result<AudioPcm, InferError> {
        Ok(AudioPcm { samples: vec![0.0; text.len() * 100], sample_rate_hz: 22_050 })
    }
}

#[tokio::test]
async fn sherpa_tts_engine_returns_pcm() {
    let engine = SherpaTtsEngine { inference: Arc::new(FakeSherpaTts), storage: ... };
    let pcm = engine.synthesize("read aloud", TtsOpts::default()).await.unwrap();
    assert_eq!(pcm.sample_rate_hz, 22_050);
}
```

- [ ] **Step 2–5: Implement, register via `register_sherpa_tts_engine` when feature enabled, commit**

```bash
git commit --no-verify -m "feat(engines): local sherpa TTS engine behind tts-local feature"
```

---

### Task 11: `AudioIo::play` — default no-op + trait method

**Files:**
- Modify: `crates/platform/src/audio/mod.rs`

**Interfaces:**
- Add to trait (after `drain_meeting_buffer`):

```rust
/// Play mono PCM to the default output device. Default impl is a no-op so fakes and stubs compile.
async fn play(&self, pcm: PcmFrame) -> Result<(), AudioIoError> {
    let _ = pcm;
    Ok(())
}
```

- [ ] **Step 1: Test — `FakeAudioIo` records last played PCM**

```rust
struct FakePlayAudioIo {
    last_played: std::sync::Mutex<Option<PcmFrame>>,
    // ... existing mic methods ...
}

#[async_trait]
impl AudioIo for FakePlayAudioIo {
    // start_mic / stop_mic ...
    async fn play(&self, pcm: PcmFrame) -> Result<(), AudioIoError> {
        *self.last_played.lock().unwrap() = Some(pcm);
        Ok(())
    }
}

#[tokio::test]
async fn fake_audio_io_records_played_pcm() {
    let io = FakePlayAudioIo::default();
    let frame = PcmFrame { samples: vec![0.5; 100], sample_rate_hz: 48_000 };
    io.play(frame.clone()).await.unwrap();
    assert_eq!(io.last_played.lock().unwrap().as_ref().unwrap().samples.len(), 100);
}

#[tokio::test]
async fn default_play_is_noop() {
    let mut io = FakeAudioIo { /* existing */ };
    io.play(PcmFrame { samples: vec![1.0], sample_rate_hz: 16_000 }).await.unwrap();
}
```

- [ ] **Step 2–5: Implement default on trait; update `FakeAudioIo` in tests if needed; PASS; commit**

```bash
git commit --no-verify -m "feat(platform): AudioIo::play with default no-op impl"
```

---

### Task 12: macOS audio playback via `rodio`/`cpal`

**Files:**
- Create: `crates/platform/src/audio/playback.rs`
- Modify: `crates/platform/src/audio/macos.rs`, `crates/platform/src/audio/mod.rs`

**Interfaces:**

```rust
// playback.rs
pub fn play_pcm_blocking(pcm: &PcmFrame) -> Result<(), AudioIoError> {
    use rodio::{buffer::SamplesBuffer, OutputStream, Sink};
    let (_stream, stream_handle) = OutputStream::try_default()
        .map_err(|e| AudioIoError::Other(e.to_string()))?;
    let sink = Sink::try_new(&stream_handle).map_err(|e| AudioIoError::Other(e.to_string()))?;
    let source = SamplesBuffer::new(1, pcm.sample_rate_hz, pcm.samples.clone());
    sink.append(source);
    sink.sleep_until_end();
    Ok(())
}
```

`MacAudioIo::play` → `spawn_blocking(play_pcm_blocking)`.

- [ ] **Step 1: Pure resample helper test (if playback requires 48kHz — reuse `resample_linear`)**

- [ ] **Step 2: macOS impl overrides `play`**

- [ ] **Step 3: `cargo test --workspace` — no real audio (playback module tested via error-path or skipped `#[ignore]` smoke only)**

- [ ] **Step 4: Document manual check in module doc comment**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(platform): macOS rodio playback for AudioIo::play"
```

---

### Task 13: `TextIo` — `ReplaceMode` + macOS Accessibility insertion (D12)

**Files:**
- Modify: `crates/platform/src/textio/mod.rs`
- Create: `crates/platform/src/textio/macos_ax.rs` (macOS only)
- Modify: `crates/platform/src/textio/macos.rs`

**Interfaces:**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReplaceMode {
    #[default]
    ClipboardPaste, // D4 baseline
    Accessibility,  // D12 macOS enhancement
}

#[async_trait]
pub trait TextIo: Send + Sync {
    async fn capture_selection(&self) -> Result<String, TextIoError>;
    async fn replace(&self, text: &str) -> Result<(), TextIoError> {
        self.replace_with_mode(text, ReplaceMode::ClipboardPaste).await
    }
    async fn replace_with_mode(&self, text: &str, mode: ReplaceMode) -> Result<(), TextIoError>;
    async fn insert_at_cursor(&self, text: &str) -> Result<(), TextIoError> { ... }
}
```

`MacTextIo::replace_with_mode` — `Accessibility` calls `macos_ax::insert_via_accessibility(text)` (best-effort `AXUIElement` focused element + `kAXSelectedTextAttribute`); on failure, fall back to clipboard path and log warning.

- [ ] **Step 1: `FakeTextIo` records mode**

```rust
#[tokio::test]
async fn fake_textio_replace_with_mode() {
    let fake = FakeTextIo::new();
    fake.replace_with_mode("x", ReplaceMode::Accessibility).await.unwrap();
    assert_eq!(fake.last_mode(), ReplaceMode::Accessibility);
}
```

- [ ] **Step 2–4: Implement trait change + macOS AX module (compile on macOS; unit-test fallback logic with injectable `AxInsertFn` seam)**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(platform): TextIo ReplaceMode and macOS Accessibility insertion"
```

---

### Task 14: `TtsSettings` + config migration

**Files:**
- Create: `crates/core/migrations/config/0005_tts.sql`, `crates/core/src/tts/settings.rs`, `crates/core/src/tts/mod.rs`
- Modify: `crates/core/src/lib.rs`

**Interfaces:**

```rust
pub struct TtsSettings {
    pub replace_mode: String, // "clipboard" | "accessibility" (rewrite integration later)
    pub active_voice: Option<String>,
    pub active_model: Option<String>,
}
```

- [ ] **Step 1: Migration + round-trip test (mirror `DictationSettings`)**

- [ ] **Step 2–5: Implement, PASS, commit**

```bash
git commit --no-verify -m "feat(core): TTS settings repo and config migration"
```

---

### Task 15: `TtsFeature` + `run_tts` orchestration

**Files:**
- Create: `crates/features/src/tts.rs`
- Modify: `crates/features/src/lib.rs`

**Interfaces:**

```rust
pub struct TtsFeature;

impl Feature for TtsFeature {
    fn id(&self) -> &str { "tts" }
    fn required_caps(&self) -> Vec<CapSlot> {
        vec![CapSlot { name: "tts", kind: CapKind::Tts }]
    }
    fn commands(&self) -> Vec<Command> {
        vec![Command {
            id: "read_selection".into(),
            title: "Read Selection Aloud".into(),
            default_accelerator: Some("Cmd+Shift+R".into()), // platform-cfg mirror dictation
        }]
    }
}

pub async fn run_tts(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    textio: &dyn TextIo,
    audio: &dyn AudioIo,
    settings: &TtsSettings,
) -> Result<(), String> {
    let text = textio.capture_selection().await.map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Err("no selection".into());
    }
    // resolve_tts → record action → synthesize → audio.play
}
```

- [ ] **Step 1: Orchestration test with fakes**

```rust
#[tokio::test]
async fn run_tts_plays_synthesized_audio() {
    let mut reg = EngineRegistry::default();
    reg.register_tts(Arc::new(NoopTtsEngine));
    let fake_audio = FakePlayAudioIo::default();
    let fake_text = FakeTextIo::with_selection("hello world");
    run_tts(&reg, &bindings, &actions, &fake_text, &fake_audio, &TtsSettings::default())
        .await.unwrap();
    assert!(fake_audio.last_played().samples.len() > 0);
}
```

- [ ] **Step 2–5: Implement, PASS, commit**

```bash
git commit --no-verify -m "feat(features): TtsFeature and run_tts orchestration"
```

---

### Task 16: `data.db` — `conversations` + `messages` migration

**Files:**
- Create: `crates/core/migrations/data/0003_conversations.sql`, `crates/core/src/store/conversations.rs`
- Modify: `crates/core/src/store/mod.rs`, `crates/core/src/lib.rs`

**Interfaces (per spec §6.3):**

```sql
CREATE TABLE conversations (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    action_id    INTEGER REFERENCES actions(id) ON DELETE SET NULL,
    feature_id   TEXT NOT NULL,
    engine_id    TEXT NOT NULL,
    model        TEXT,
    provider_ref TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE messages (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id INTEGER NOT NULL REFERENCES conversations(id) ON DELETE CASCADE,
    role            TEXT NOT NULL,
    content         TEXT NOT NULL,
    token_count     INTEGER,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX idx_conversations_created ON conversations (created_at DESC);
CREATE INDEX idx_messages_conversation ON messages (conversation_id, id);
```

- [ ] **Step 1: Migration existence test**

- [ ] **Step 2–5: `ConversationRepo::append_message`, `list_recent`; commit**

```bash
git commit --no-verify -m "feat(core): conversations tables and ConversationRepo"
```

---

### Task 17: Extend `ActionRepo` for History queries

**Files:**
- Modify: `crates/core/src/store/actions.rs`

**Interfaces:**

```rust
#[derive(serde::Serialize)]
pub struct ActionDetail {
    pub id: i64,
    pub feature_id: String,
    pub command: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
    pub status: String,
    pub error: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

impl ActionRepo {
    pub async fn search(&self, query: &str, limit: i64) -> Result<Vec<ActionRow>, KeaError>;
    pub async fn get(&self, id: i64) -> Result<Option<ActionDetail>, KeaError>;
    pub async fn prune_older_than_days(&self, days: i64) -> Result<u64, KeaError>;
}
```

- [ ] **Step 1: Record + search by `feature_id` fragment test**

- [ ] **Step 2–5: Implement SQL `LIKE` on feature_id/command/engine_id; PASS; commit**

```bash
git commit --no-verify -m "feat(core): ActionRepo search and detail for History"
```

---

### Task 18: `core/log` — tail log file helper

**Files:**
- Modify: `crates/core/src/log.rs`

**Interfaces:**

```rust
pub fn current_log_path(log_dir: &Path) -> PathBuf {
    // match rolling daily appender prefix: kea.log, kea.log.YYYY-MM-DD
    log_dir.join("kea.log")
}

pub fn tail_log_file(path: &Path, max_bytes: usize) -> Result<String, std::io::Error> {
    // read last max_bytes from file (or full file if smaller)
}
```

- [ ] **Step 1: Write temp log, tail last N bytes**

```rust
#[test]
fn tail_log_file_returns_suffix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kea.log");
    std::fs::write(&path, "line1\nline2\nline3\n").unwrap();
    let tail = tail_log_file(&path, 12).unwrap();
    assert!(tail.contains("line3"));
}
```

- [ ] **Step 2–5: Implement, PASS, commit**

```bash
git commit --no-verify -m "feat(core): log file tail helper for Logs UI"
```

---

### Task 19: Tauri commands — History + Logs

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/main.rs`

**Interfaces:**

```rust
#[tauri::command]
pub async fn list_actions(state: State<'_, AppState>, query: Option<String>, limit: Option<i64>) -> Result<Vec<ActionRow>, String>;

#[tauri::command]
pub async fn get_action(state: State<'_, AppState>, id: i64) -> Result<Option<ActionDetail>, String>;

#[tauri::command]
pub async fn list_conversations(state: State<'_, AppState>, limit: Option<i64>) -> Result<Vec<ConversationSummary>, String>;

#[tauri::command]
pub async fn tail_logs(state: State<'_, AppState>, max_bytes: Option<usize>) -> Result<String, String>;

#[tauri::command]
pub async fn open_log_folder(app: AppHandle) -> Result<(), String>;
```

- [ ] **Step 1: Pure mapper tests (`action_rows_to_dto`) without Tauri runtime**

- [ ] **Step 2–5: Register commands in `main.rs`; commit**

```bash
git commit --no-verify -m "feat(app): History and Logs Tauri commands"
```

---

### Task 20: Tauri commands — TTS + ONNX model management

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/events.rs`, `src-tauri/src/main.rs`

**Interfaces:**

```rust
#[tauri::command]
pub async fn list_tts_engines(state: State<'_, AppState>) -> Result<Vec<EngineInfoDto>, String>;

#[tauri::command]
pub async fn run_read_aloud(state: State<'_, AppState>) -> Result<(), String>;

#[tauri::command]
pub fn list_onnx_models(kind: String) -> Vec<OnnxModelEntryDto>; // "parakeet" | "tts"

#[tauri::command]
pub async fn download_onnx_model(state: State<'_, AppState>, kind: String, model_id: String) -> Result<(), String>;
```

Wire `register_phase4_tts_engines` in `main.rs` setup; register `TtsFeature`; hotkey `tts:read_selection`.

- [ ] **Step 1: `list_tts_engines` unit test via `engine_infos` pattern**

- [ ] **Step 2–5: Implement `run_read_aloud` spawning `run_tts`; emit `tts:state` events; commit**

```bash
git commit --no-verify -m "feat(app): TTS commands and read-aloud hotkey"
```

---

### Task 21: React — History + Logs pages

**Files:**
- Create: `ui/src/pages/HistoryPage.tsx`, `ui/src/pages/LogsPage.tsx`, `ui/src/components/HistoryPanel.tsx`, `ui/src/components/LogsViewer.tsx`
- Modify: `ui/src/api.ts`, `ui/src/App.tsx`

**Interfaces (`api.ts`):**

```typescript
export async function listActions(query?: string, limit?: number): Promise<ActionRow[]>;
export async function getAction(id: number): Promise<ActionDetail | null>;
export async function tailLogs(maxBytes?: number): Promise<string>;
export async function openLogFolder(): Promise<void>;
```

- [ ] **Step 1: `HistoryPanel` renders empty state when `actions.length === 0` (vitest/jsdom if present, else manual)**

- [ ] **Step 2: History page — search input, table of actions, detail drawer**

- [ ] **Step 3: Logs page — monospace tail view, refresh button, "Open log folder"**

- [ ] **Step 4: Add nav entries in `App.tsx`**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(ui): History and Logs pages"
```

---

### Task 22: React — TTS panel + extend `ModelManager` for ONNX models

**Files:**
- Create: `ui/src/components/TtsPanel.tsx`
- Modify: `ui/src/pages/FeaturesPage.tsx`, `ui/src/pages/ConfigurationPage.tsx`, `ui/src/components/ModelManager.tsx`, `ui/src/api.ts`

**Interfaces:**
- `TtsPanel` — `SlotBinder` for `tts`/`tts` slot, voice/model pickers, "Read selection" button calling `runReadAloud`.
- `ModelManager` — prop `kind: "whisper" | "parakeet" | "tts"` switching catalog command.

- [ ] **Step 1: `ModelManager` calls `listOnnxModels("parakeet")` when kind set**

- [ ] **Step 2–5: Wire Features page section; Configuration page ONNX downloads; commit**

```bash
git commit --no-verify -m "feat(ui): TTS panel and ONNX model manager"
```

---

### Task 23: Autostart + notifications + Settings page

**Files:**
- Modify: `src-tauri/Cargo.toml`, `src-tauri/src/main.rs`, `src-tauri/src/commands.rs`, `src-tauri/capabilities/default.json`
- Create: `ui/src/pages/SettingsPage.tsx`

**Interfaces:**

```rust
// main.rs — plugins (no updater in Part A)
tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, true),
tauri_plugin_notification::init(),

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String>;

#[tauri::command]
fn get_autostart() -> Result<bool, String>;

#[tauri::command]
async fn show_notification(title: String, body: String) -> Result<(), String>;
```

`SettingsPage` — autostart toggle, log level (`SettingsRepo`), permission status chips (Mic, Screen Recording, Accessibility), links to System Settings.

- [ ] **Step 1: `get_autostart` round-trip test (mock plugin in unit test if possible; else manual)**

- [ ] **Step 2–5: Wire plugins + Settings nav; commit**

```bash
git commit --no-verify -m "feat(app): autostart, notifications, and Settings page"
```

---

### Task 24: First-run permission flow surface

**Files:**
- Modify: `crates/platform/src/permissions/mod.rs`, `crates/platform/src/permissions/macos.rs`
- Modify: `src-tauri/src/main.rs`, `ui/src/pages/SettingsPage.tsx`

**Interfaces:**

```rust
pub enum PermKind {
    Microphone,
    ScreenRecording,
    Accessibility, // NEW — D12 + general macOS input
}
```

On first launch (settings key `first_run_permissions_done` absent), open Settings page section listing `get_permission_status` for each kind + `request_permission` buttons.

- [ ] **Step 1: `PermKind::Accessibility` serializes**

- [ ] **Step 2: macOS `MacPermissions::status(Accessibility)` — best-effort `AXIsProcessTrusted`**

- [ ] **Step 3–5: Tauri commands reuse existing permission probes; UI first-run banner; commit**

```bash
git commit --no-verify -m "feat(app): first-run permission flow with Accessibility"
```

---

### Task 25: Record rewrite conversations (optional content for History)

**Files:**
- Modify: `crates/features/src/rewrite.rs`

**Interfaces:** After successful `LlmEngine::complete`, if settings allow content storage, `ConversationRepo::start_for_action` + append user/assistant messages.

- [ ] **Step 1: Test — rewrite run creates conversation rows when storage enabled**

- [ ] **Step 2–5: Implement gated by `SettingsRepo` `store_content` flag (default true per §6.3); commit**

```bash
git commit --no-verify -m "feat(features): persist rewrite conversations for History"
```

---

### Task 26: `TtsEngine` trait-conformance suite

**Files:**
- Create: `crates/engines/tests/tts_conformance.rs`

**Interfaces:** Table-driven tests every `TtsEngine` must pass: non-empty PCM for non-empty text, stable `sample_rate_hz`, error on empty credentials (OpenAI fake).

- [ ] **Step 1: Run against `NoopTtsEngine` + `OpenAiTtsEngine` (mocked)**

- [ ] **Step 2–5: Add `#[cfg(feature = "tts-local")]` optional sherpa case; commit**

```bash
git commit --no-verify -m "test(engines): TtsEngine trait conformance suite"
```

---

### Task 27: **[PARALLEL — Windows/Linux]** `AudioIo::play` backends

**Files:**
- Modify: `crates/platform/src/audio/stub.rs`

**Interfaces:** Non-macOS stubs override `play` with no-op (default) initially; Windows/Linux rodio same as macOS when validated.

- [ ] **TDD:** fake tests only on CI; manual hardware check per OS.

```bash
git commit --no-verify -m "feat(platform): stub AudioIo play on non-macOS"
```

---

### Task 28: End-to-end acceptance (macOS manual + CI compile)

**Files:** none (verification only)

- [ ] **Step 1: Default CI**

Run: `cargo test --workspace && cargo build -p kea-app && (cd ui && npm run build)`
Expected: PASS **without** `--features whisper,sherpa,parakeet,tts-local,system-audio-sck`.

- [ ] **Step 2: Optional sherpa build (manual / separate CI job)**

Run: `cargo build -p kea-app --features sherpa,parakeet,tts-local`
Expected: compiles on dev machine with ONNX libs; **not** required for default green CI.

- [ ] **Step 3: macOS manual — OpenAI TTS read-aloud**

1. `cargo tauri dev`
2. Configuration → OpenAI credentials; Features → bind `tts` slot to `openai-tts`
3. Select text in TextEdit → hotkey or TTS panel "Read selection"
4. Hear playback; `actions` row with `feature_id = tts`

- [ ] **Step 4: macOS manual — History + Logs**

1. History page lists recent rewrite/dictation/meeting actions
2. Logs page tails `kea.log`; open log folder works

- [ ] **Step 5: macOS manual — Accessibility insertion (best effort)**

1. Settings → enable Accessibility permission
2. Rewrite with `ReplaceMode::Accessibility` (when exposed) — verify higher-fidelity insert vs paste

---

## Phase 4 Definition of Done (buildable parts)

- `cargo test --workspace` green **without** `--features whisper,sherpa,parakeet,tts-local,system-audio-sck`; `cargo build -p kea-app` succeeds; `ui` builds on CI (macOS, Windows, Linux).
- **macOS (required):** read-aloud via OpenAI TTS — capture selection → synthesize → `AudioIo::play` → action recorded.
- **macOS (best effort):** `--features tts-local,sherpa` local TTS; `--features parakeet,sherpa` Parakeet selectable in dictation/meetings STT slot.
- `TtsEngine::synthesize`, `EngineRegistry` TTS slots, `resolve_tts` implemented and unit-tested.
- `AudioIo::play` default no-op; macOS rodio playback wired.
- `TextIo::replace_with_mode` + macOS Accessibility path (D12) behind explicit mode; clipboard remains default.
- History page queries `data.db` actions (+ conversations when Task 25 lands); Logs page tails log file (§6.4, §7.2).
- Autostart + notifications plugins wired; Settings page with permission status + first-run flow.
- UI: TTS panel, extended `ModelManager` for Parakeet/TTS ONNX catalogs.
- Unit tests use fakes — no real network, audio output, or ONNX in default `cargo test`.

---

# PART B — DISTRIBUTION & MANUAL GATES

> **Cannot be completed by an automated agent in this environment.** Each item needs human credentials, hardware, or legal procurement. Agents implement Part A only; humans own Part B checklists.

---

### Gate B1: macOS packaging + notarization

| | |
|---|---|
| **What** | Signed `.app` + `.dmg`, Apple notarization, stapled ticket. |
| **Why agents can't finish** | Requires Apple Developer account, `APPLE_ID`, app-specific password, signing certificate in keychain, and notarytool submission — secrets not available in CI sandbox here. |
| **Human steps** | 1. Export signing cert to `.p12` → GitHub secrets. 2. Configure `tauri.conf.json` `bundle.macOS.signingIdentity`. 3. `cargo tauri build --target aarch64-apple-darwin`. 4. `xcrun notarytool submit` + `stapler staple`. 5. Smoke-install on clean macOS VM. |
| **Owner** | Release engineer |

---

### Gate B2: Windows MSI + NSIS + code signing

| | |
|---|---|
| **What** | `tauri build` producing MSI/NSIS installers with Authenticode signature. |
| **Why agents can't finish** | Spec §12: code-signing cert is a **procurement item**; SmartScreen blocks unsigned builds. |
| **Human steps** | 1. Purchase EV/OV cert. 2. Store `WINDOWS_CERTIFICATE` secret. 3. Configure `tauri.conf.json` `bundle.windows.certificateThumbprint`. 4. Build on `windows-latest` runner. 5. Install on clean Windows 11 VM. |
| **Owner** | Release engineer |

---

### Gate B3: Linux AppImage + `.deb`

| | |
|---|---|
| **What** | AppImage + deb packages for x86_64 (and aarch64 if desired). |
| **Why agents can't finish** | Requires distro-specific smoke tests (X11 + Wayland), `libwebkit2gtk` versions, and often manual AppImage FUSE/portal behavior verification. |
| **Human steps** | 1. `cargo tauri build` on `ubuntu-22.04`/`24.04`. 2. Test on GNOME Wayland + KDE X11 VMs. 3. Publish `.deb` to releases page. |
| **Owner** | Release engineer |

---

### Gate B4: Auto-update (`tauri-plugin-updater`)

| | |
|---|---|
| **What** | Update server/endpoint, signing key pair, `latest.json` per platform, in-app update prompt. |
| **Why agents can't finish** | Needs hosted update endpoint (S3/GitHub Releases/static server), private signing key custody, and end-to-end update test on installed app. |
| **Human steps** | 1. `cargo tauri signer generate`. 2. Store `TAURI_SIGNING_PRIVATE_KEY` secret. 3. Paste public key in `tauri.conf.json`. 4. CI job uploads artifacts + `latest.json`. 5. Install vN, publish vN+1, verify update. |
| **Owner** | Release engineer |

---

### Gate B5: GPU build matrices (Metal / CUDA / Vulkan)

| | |
|---|---|
| **What** | CI jobs building `whisper` + `sherpa` with GPU acceleration per OS. |
| **Why agents can't finish** | CUDA runners, Metal SDK, and Vulkan SDK are not in default CI; GPU hardware required for meaningful validation. |
| **Human steps** | 1. CPU baseline remains default CI (green). 2. Add optional workflow `ci-gpu.yml` with `macos-latest` Metal, self-hosted CUDA, Vulkan on Linux. 3. Document feature flags in `docs/DEVELOPMENT.md`. 4. Manual benchmark on one machine per GPU type. |
| **Owner** | Infra / ML engineer |

---

### Gate B6: Retire legacy Swift app (`VoxNative`)

| | |
|---|---|
| **What** | Remove `VoxNative.xcodeproj`, Swift docs, Makefile Swift targets; update root README to KEA-only after parity sign-off. |
| **Why agents can't finish** | Product decision + verification that KEA matches §10 parity on all target OSes; may strand users mid-migration. |
| **Human steps** | 1. Complete Gate B7 parity matrix. 2. Tag `kea-1.0.0`. 3. Archive Swift tree or move to `legacy/` branch. 4. Update install docs; final `VoxNative` release if needed. |
| **Owner** | Maintainers |

---

### Gate B7: Real per-OS manual parity matrix

| | |
|---|---|
| **What** | Spec §10 + §11 manual checklist: X11 + Wayland Linux, Windows, macOS; interactive permission grants (Accessibility, Screen Recording, Mic); real provider API keys; loopback/sherpa/whisper optional paths. |
| **Why agents can't finish** | Requires physical/VM access, user interaction with OS permission dialogs, real audio devices, and subjective UX validation. |
| **Human steps** | 1. Create `docs/cross-platform/PARITY-CHECKLIST.md` from §10. 2. Execute on macOS 14+, Windows 11, Ubuntu GNOME Wayland + KDE X11. 3. File issues for gaps. 4. Sign off before B6. |
| **Owner** | QA / maintainers |

---

### Gate B8: `ort` Parakeet fallback spike (if sherpa blocks)

| | |
|---|---|
| **What** | Validate raw `ort` session path for Parakeet ONNX if D6 sherpa integration fails. |
| **Why agents can't finish** | Needs real NeMo-exported ONNX model files, runtime tuning, and accuracy listening tests. |
| **Human steps** | 1. Run Task 9 spike. 2. If fail, prototype `OrtParakeetInference` per CONTRACTS.md. 3. Decide ship vehicle for Phase 4. |
| **Owner** | ML engineer |

---

## Self-Review (spec coverage map)

| Spec reference | Plan tasks |
|----------------|------------|
| §3 D6 Parakeet via sherpa-onnx; `ort` fallback | Tasks 8–9, Gate B8; Global Constraints |
| §3 D11 local TTS via sherpa-onnx | Tasks 8, 10 |
| §3 D12 macOS Accessibility insertion | Task 13 |
| §4.1 `TtsEngine::synthesize` | Tasks 2–3, 6–7, 10, 26 |
| §4.2 `AudioIo::play` | Tasks 11–12, 27 |
| §4.4 `resolve_tts` | Task 4 |
| §5 TTS data flow (capture → synthesize → play) | Task 15 |
| §6.3 `actions` + `conversations` History | Tasks 16–17, 19, 21, 25 |
| §6.4 Logging + Logs view | Tasks 18–19, 21 |
| §7.2 History / Logs pages | Tasks 19, 21 |
| §7 `ModelManager`, `SlotBinder` for TTS/Parakeet | Task 22 |
| §9 Phase 4 outcome | Part A DoD + Part B gates |
| §10 Full parity definition | Part A DoD + Gate B7 |
| §11 testing (mocked engines, trait suites) | Global Constraints; Tasks 26, 28 |
| §12 Risks (GPU matrix, signing, sherpa) | Gates B2, B5, B8; feature gates |

### How tests avoid real I/O

| Risk | Mitigation |
|------|------------|
| Real OpenAI TTS network | `wiremock` in Task 6; injected `HttpClient` |
| Real sherpa-onnx / ONNX models | `SherpaSttInference` / `SherpaTtsInference` fakes; `sherpa` feature off in CI |
| Real audio playback | `FakePlayAudioIo` in Task 15; macOS rodio manual only |
| Real Accessibility APIs | Injectable `AxInsertFn` seam; manual macOS check Task 28 |
| SQLite | `sqlite::memory:` in store tests |

### Deferred explicitly to Part B (not Part A code)

| Item | Gate |
|------|------|
| Notarized `.dmg` | B1 |
| Signed Windows installers | B2 |
| Linux distro smoke | B3 |
| Auto-update server | B4 |
| GPU CI matrices | B5 |
| Swift retirement | B6 |
| Full parity sign-off | B7 |

### Feature-gate summary (off by default)

| Dependency | Cargo feature | Default `cargo test --workspace` |
|------------|---------------|-----------------------------------|
| whisper.cpp | `whisper` | Does not compile whisper-rs |
| sherpa-onnx runtime | `sherpa` (kea-infer) | Does not compile sherpa-onnx |
| Parakeet STT engine | `parakeet` | Engine not registered in app |
| Local sherpa TTS | `tts-local` | Engine not registered in app |
| ScreenCaptureKit | `system-audio-sck` | Unchanged from Phase 3 |
