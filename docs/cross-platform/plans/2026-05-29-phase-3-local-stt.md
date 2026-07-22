# Cross-Platform Rewrite — Phase 3 (Local / offline speech-to-text) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add fully offline speech-to-text to Vox on macOS, Windows, and Linux. Introduce a new `vox-infer` workspace member that runs whisper.cpp (via `whisper-rs`) behind the existing `core::speech::Transcriber` trait, ship a model registry + downloader + on-disk model management, wire GPU acceleration through Cargo feature flags (Metal / CUDA / Vulkan with a guaranteed CPU baseline), let the engine factory in `src-tauri` build a `LocalTranscriber` when the user selects `SpeechEngineKind::WhisperLocal`, and give the UI a model-management screen (list, download with live progress, select active model).

**Architecture:** Phase 0–2 are built: the Cargo workspace (`app/`), `vox-core` with `settings`/`secrets`/`rewrite`/`speech`, `vox-platform` with `hotkeys`/`textio`/`audio`, the `vox` Tauri shell, and the dictation flow that captures mic audio (`platform::audio`) and feeds a `Box<dyn Transcriber>` (today only `RemoteTranscriber`). This phase adds `vox-infer` (a new, platform-agnostic-API crate whose backend is whisper.cpp). `LocalTranscriber` implements `core::speech::Transcriber` so it drops into the same dictation pipeline; `ModelManager` owns the registry and the models directory. The engine factory — which must depend on **both** `vox-core` and `vox-infer` — lives in `src-tauri`. Only `src-tauri` uses `tokio`; `vox-core` stays runtime-agnostic via the async trait, and `vox-infer`'s synchronous whisper inference is wrapped so `src-tauri` can call it. Downloads use `reqwest` streaming with a progress callback surfaced to the UI as `model:progress` events.

**Tech Stack:** Rust (edition 2021), `whisper-rs` (whisper.cpp bindings), `reqwest` (features `["json","rustls-tls","stream"]`), `futures-util` (stream consumption), `tokio` (in `src-tauri` only), `serde`/`serde_json`, `thiserror`, `tempfile` (dev), React 18 + TypeScript + Vite, GitHub Actions (macOS Metal, Windows/Linux CUDA-or-Vulkan, CPU baseline on all three).

**Reference spec:** `docs/cross-platform/2026-05-29-cross-platform-rewrite-design.md` (Phase 3, Decision D1). Canonical interfaces: `docs/cross-platform/plans/CONTRACTS.md`.

---

## File Structure

The offline ASR engine lives in a new workspace member, `app/crates/infer`
(package name `vox-infer`), kept separate from `vox-core` so the heavy
whisper.cpp build (and its GPU feature flags) does not leak into the
platform-agnostic core.

- `app/Cargo.toml` — add `crates/infer` to `members`; add `vox-infer` to `[workspace.dependencies]` for `src-tauri` to consume.
- `app/crates/infer/Cargo.toml` — the `vox-infer` crate manifest; declares `whisper-rs`, the GPU feature flags, and `reqwest`/`futures-util` for downloads.
- `app/crates/infer/src/lib.rs` — re-exports `model_manager` and `local_transcriber`.
- `app/crates/infer/src/model_manager.rs` — `WhisperModel`, the `tiny..large-v3` registry, and `ModelManager` (path resolution, `is_downloaded`, async streaming `download`).
- `app/crates/infer/src/local_transcriber.rs` — `LocalTranscriber::load` + its `core::speech::Transcriber` impl over whisper.cpp.
- `app/crates/core/src/settings.rs` — add the Phase 3 field `local_model_id`; bump `schema_version` to 4.
- `app/src-tauri/Cargo.toml` — depend on `vox-infer`; expose the GPU feature flags that forward into `vox-infer`.
- `app/src-tauri/src/engine.rs` — engine factory: `build_transcriber(&Settings, api_key, &ModelManager) -> Box<dyn Transcriber>` selecting Local vs Remote from `SpeechEngineKind`.
- `app/src-tauri/src/commands.rs` — add `list_models` and `download_model` commands (the latter emits `model:progress`).
- `app/src-tauri/src/main.rs` — register the two new commands.
- `app/ui/src/ModelManager.tsx` — model-management UI (list, download w/ progress, select active model).
- `app/ui/src/App.tsx` — mount the model-management screen.
- `.github/workflows/app-ci.yml` — add the CPU-baseline test/build (all OSes) plus per-OS GPU build jobs (Metal on macOS, Vulkan on Windows/Linux).

Each file keeps one responsibility: `model_manager.rs` knows registry + disk +
download only; `local_transcriber.rs` knows whisper.cpp inference only;
`engine.rs` only chooses an engine; `commands.rs` only adapts to Tauri.

---

## Prerequisites (one-time, not committed)

- [ ] **Step 0: Verify the whisper.cpp build toolchain is present**

`whisper-rs` compiles whisper.cpp from source, so a C/C++ toolchain and CMake
must be installed. Run:
```bash
cmake --version
cc --version || clang --version
rustc --version && cargo --version
```
Expected: `cmake` ≥ 3.x and a C compiler print versions. If missing:
- macOS: `xcode-select --install` (clang) and `brew install cmake`.
- Ubuntu: `sudo apt-get update && sudo apt-get install -y cmake build-essential`.
- Windows: install Visual Studio Build Tools (C++ workload) and CMake.

For the **optional** GPU smoke check only (not required for unit tests / CI
baseline): macOS Metal needs no extra SDK; Vulkan needs the LunarG Vulkan SDK;
CUDA needs the NVIDIA CUDA Toolkit. The CPU baseline requires none of these.

---

## Task 1: Add the `vox-infer` workspace member

**Files:**
- Modify: `app/Cargo.toml`
- Create: `app/crates/infer/Cargo.toml`, `app/crates/infer/src/lib.rs`

- [ ] **Step 1: Add the member + workspace dependencies**

Edit `app/Cargo.toml`. Add `"crates/infer"` to `members` and add the listed
entries to `[workspace.dependencies]` (merge with the existing keys; do not
remove `serde`/`serde_json`):
```toml
[workspace]
resolver = "2"
members = ["crates/core", "crates/platform", "crates/infer", "src-tauri"]

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
vox-infer = { path = "crates/infer" }
whisper-rs = "0.12"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "stream"] }
futures-util = "0.3"
thiserror = "1"
```

- [ ] **Step 2: Create the `vox-infer` manifest with GPU feature flags**

Create `app/crates/infer/Cargo.toml`. The `cpu` feature is the default and
guaranteed baseline; each GPU feature forwards to the matching `whisper-rs`
backend feature. `vox-core` is a path dependency because `LocalTranscriber`
implements `core::speech::Transcriber` and reuses `SpeechError`:
```toml
[package]
name = "vox-infer"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
vox-core = { path = "../core" }
whisper-rs = { workspace = true }
reqwest = { workspace = true }
futures-util = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }
async-trait = "0.1"

[dev-dependencies]
tempfile = "3"

[features]
# CPU is the guaranteed baseline and the default everywhere.
default = ["cpu"]
cpu = []
# GPU backends forward to whisper-rs's corresponding features.
metal = ["whisper-rs/metal"]
cuda = ["whisper-rs/cuda"]
vulkan = ["whisper-rs/vulkan"]
```

- [ ] **Step 3: Create the crate root**

Create `app/crates/infer/src/lib.rs`:
```rust
//! Offline speech-to-text via whisper.cpp (whisper-rs). Decision D1.
//!
//! `model_manager` owns the model registry, on-disk paths, and downloads.
//! `local_transcriber` runs inference behind `core::speech::Transcriber`.
pub mod model_manager;
pub mod local_transcriber;
```

- [ ] **Step 4: Verify the workspace recognizes the member**

Run:
```bash
cargo metadata --manifest-path app/Cargo.toml --no-deps --format-version 1 | grep -q vox-infer && echo "member OK"
```
Expected: prints `member OK` (the crate is part of the workspace). A full
`cargo build` is deferred until `model_manager.rs`/`local_transcriber.rs` exist;
the empty `pub mod` lines above reference modules created in Tasks 2–3.

- [ ] **Step 5: Commit**

```bash
git add app/Cargo.toml app/crates/infer/Cargo.toml app/crates/infer/src/lib.rs
git commit -m "feat(infer): add vox-infer workspace member with GPU feature flags"
```

---

## Task 2: Model registry + ModelManager (registry, paths, is_downloaded)

**Files:**
- Create: `app/crates/infer/src/model_manager.rs`
- Test: in-file `#[cfg(test)]` module in `model_manager.rs`

- [ ] **Step 1: Write the failing tests**

Create `app/crates/infer/src/model_manager.rs` with the types, a stubbed
registry/manager that does *not yet compile-pass the assertions*, and the tests.
Write this exact file:
```rust
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use vox_core::speech::SpeechError;

/// A downloadable whisper.cpp GGML model in the registry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WhisperModel {
    pub id: String,
    pub display_name: String,
    pub url: String,
    pub size_bytes: u64,
}

/// Owns the on-disk models directory and the canonical model registry.
pub struct ModelManager {
    models_dir: PathBuf,
}

impl ModelManager {
    pub fn new(models_dir: PathBuf) -> Self {
        Self { models_dir }
    }

    /// The built-in registry, tiny..large-v3. URLs point at the canonical
    /// ggml GGUF/bin files published for whisper.cpp.
    pub fn registry() -> Vec<WhisperModel> {
        fn m(id: &str, name: &str, file: &str, size: u64) -> WhisperModel {
            WhisperModel {
                id: id.to_string(),
                display_name: name.to_string(),
                url: format!(
                    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}"
                ),
                size_bytes: size,
            }
        }
        vec![
            m("tiny", "Tiny", "ggml-tiny.bin", 77_700_000),
            m("base", "Base", "ggml-base.bin", 147_900_000),
            m("small", "Small", "ggml-small.bin", 487_600_000),
            m("medium", "Medium", "ggml-medium.bin", 1_533_000_000),
            m("large-v3", "Large v3", "ggml-large-v3.bin", 3_095_000_000),
        ]
    }

    /// Absolute path the model with `model_id` is (or would be) stored at.
    pub fn path_for(&self, model_id: &str) -> PathBuf {
        self.models_dir.join(format!("ggml-{model_id}.bin"))
    }

    /// True if the model file already exists on disk.
    pub fn is_downloaded(&self, model_id: &str) -> bool {
        self.path_for(model_id).exists()
    }

    fn url_for(model_id: &str) -> Result<String, SpeechError> {
        Self::registry()
            .into_iter()
            .find(|m| m.id == model_id)
            .map(|m| m.url)
            .ok_or_else(|| SpeechError::Config(format!("unknown model id: {model_id}")))
    }

    fn ensure_dir(dir: &Path) -> Result<(), SpeechError> {
        std::fs::create_dir_all(dir)
            .map_err(|e| SpeechError::Inference(format!("create models dir: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_spans_tiny_to_large_v3() {
        let ids: Vec<String> = ModelManager::registry().into_iter().map(|m| m.id).collect();
        assert_eq!(ids.first().map(String::as_str), Some("tiny"));
        assert_eq!(ids.last().map(String::as_str), Some("large-v3"));
        assert!(ids.contains(&"base".to_string()));
        // every entry has a non-empty url and a plausible size
        for model in ModelManager::registry() {
            assert!(model.url.starts_with("https://"));
            assert!(model.size_bytes > 0);
        }
    }

    #[test]
    fn path_for_is_under_models_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf());
        let path = mgr.path_for("base");
        assert_eq!(path, dir.path().join("ggml-base.bin"));
    }

    #[test]
    fn is_downloaded_reflects_disk_state() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf());
        assert!(!mgr.is_downloaded("base"));
        std::fs::write(mgr.path_for("base"), b"fake-ggml").unwrap();
        assert!(mgr.is_downloaded("base"));
    }

    #[test]
    fn url_for_unknown_model_is_config_error() {
        let err = ModelManager::url_for("nonexistent").unwrap_err();
        assert!(matches!(err, SpeechError::Config(_)));
    }
}
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-infer model_manager`
Expected: all four tests PASS. (This task is registry + path logic only; the
`download` method and its network behavior arrive in Task 3, so `url_for`/
`ensure_dir` are present but only `url_for` is exercised here. `ensure_dir` is
deliberately referenced by `download` next; if the dead-code warning is noisy,
it is resolved in Task 3 when `download` calls it.)

- [ ] **Step 3: Commit**

```bash
git add app/crates/infer/src/model_manager.rs
git commit -m "feat(infer): add WhisperModel registry + ModelManager paths/is_downloaded"
```

---

## Task 3: Streaming model download with progress

**Files:**
- Modify: `app/crates/infer/src/model_manager.rs`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `model_manager.rs`. The test serves bytes from a
tiny local TCP server (std-only, no extra deps) so the download path and the
progress callback are exercised without the network. Add:
```rust
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    /// Minimal one-shot HTTP/1.1 server returning `body` with Content-Length.
    /// Returns the bound base URL (e.g. "http://127.0.0.1:PORT").
    fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf); // consume request line/headers
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes());
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn download_streams_bytes_and_reports_progress() {
        let body = vec![7u8; 4096];
        let base = serve_once(body.clone());

        let dir = tempfile::tempdir().unwrap();
        let mgr = ModelManager::new(dir.path().to_path_buf());
        let dest = mgr.path_for("base");

        let max_progress = Arc::new(AtomicU64::new(0));
        let max_clone = Arc::clone(&max_progress);

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            mgr.download_from(&base, &dest, move |frac| {
                // store the highest fraction seen, scaled to permille
                let permille = (frac * 1000.0) as u64;
                max_clone.fetch_max(permille, Ordering::SeqCst);
            })
            .await
            .unwrap();
        });

        assert!(dest.exists());
        assert_eq!(std::fs::read(&dest).unwrap(), body);
        // progress reached completion (1.0 == 1000 permille)
        assert_eq!(max_progress.load(Ordering::SeqCst), 1000);
    }
```
Note: the test uses `tokio` as a dev-dependency to drive the async `download`.
Add it to `app/crates/infer/Cargo.toml` `[dev-dependencies]`:
```toml
tokio = { version = "1", features = ["rt", "macros"] }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-infer download`
Expected: FAIL — `download_from` (and the public `download`) are not defined yet.

- [ ] **Step 3: Write the streaming download implementation**

Add the public `download` plus the testable `download_from` to the `impl
ModelManager` block (above the `tests` module) in `model_manager.rs`:
```rust
    /// Download `model_id` into the models directory, invoking `on_progress`
    /// with a 0.0..=1.0 fraction as bytes arrive. Resolves the URL from the
    /// registry, then delegates to `download_from`.
    pub async fn download(
        &self,
        model_id: &str,
        on_progress: impl Fn(f64) + Send,
    ) -> Result<(), SpeechError> {
        let url = Self::url_for(model_id)?;
        let dest = self.path_for(model_id);
        self.download_from(&url, &dest, on_progress).await
    }

    /// Stream `url` to `dest` (atomically via a `.part` file), reporting
    /// progress. Separated from `download` so tests can target a local URL.
    pub async fn download_from(
        &self,
        url: &str,
        dest: &Path,
        on_progress: impl Fn(f64) + Send,
    ) -> Result<(), SpeechError> {
        use futures_util::StreamExt;

        if let Some(parent) = dest.parent() {
            Self::ensure_dir(parent)?;
        }

        let response = reqwest::get(url)
            .await
            .map_err(|e| SpeechError::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(SpeechError::Http(
                response.status().as_u16(),
                response.status().to_string(),
            ));
        }
        let total = response.content_length().unwrap_or(0);

        let part = dest.with_extension("part");
        let mut file = std::fs::File::create(&part)
            .map_err(|e| SpeechError::Inference(format!("create part file: {e}")))?;

        let mut downloaded: u64 = 0;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| SpeechError::Network(e.to_string()))?;
            use std::io::Write as _;
            file.write_all(&chunk)
                .map_err(|e| SpeechError::Inference(format!("write chunk: {e}")))?;
            downloaded += chunk.len() as u64;
            if total > 0 {
                on_progress((downloaded as f64 / total as f64).min(1.0));
            }
        }
        file.flush()
            .map_err(|e| SpeechError::Inference(format!("flush: {e}")))?;
        drop(file);

        std::fs::rename(&part, dest)
            .map_err(|e| SpeechError::Inference(format!("finalize model: {e}")))?;
        // Guarantee a final 1.0 even if Content-Length was absent.
        on_progress(1.0);
        Ok(())
    }
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-infer download`
Expected: `download_streams_bytes_and_reports_progress` PASSES — the file is
written atomically, contents match, and progress reaches `1.0`.

- [ ] **Step 5: Run the full module test suite**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-infer model_manager`
Expected: all five `model_manager` tests PASS.

- [ ] **Step 6: Commit**

```bash
git add app/crates/infer/src/model_manager.rs app/crates/infer/Cargo.toml
git commit -m "feat(infer): add streaming model download with progress callback"
```

---

## Task 4: LocalTranscriber over whisper.cpp

**Files:**
- Create: `app/crates/infer/src/local_transcriber.rs`
- Test: in-file `#[cfg(test)]` module in `local_transcriber.rs`

- [ ] **Step 1: Write the failing test**

The real whisper inference is a **manual smoke check** (Step 5 below); CI must
not require a model file. So the unit test asserts the error path: loading a
non-existent model returns a `SpeechError`. Create
`app/crates/infer/src/local_transcriber.rs`:
```rust
use std::path::PathBuf;

use async_trait::async_trait;
use vox_core::speech::{
    SpeechError, TranscriptionRequest, TranscriptionResult, Transcriber,
};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

/// Offline transcriber backed by a loaded whisper.cpp model (Decision D1).
pub struct LocalTranscriber {
    context: WhisperContext,
}

impl LocalTranscriber {
    /// Load the GGML model at `model_path` into a whisper context.
    pub fn load(model_path: PathBuf) -> Result<Self, SpeechError> {
        if !model_path.exists() {
            return Err(SpeechError::Config(format!(
                "model file not found: {}",
                model_path.display()
            )));
        }
        let path = model_path
            .to_str()
            .ok_or_else(|| SpeechError::Config("model path is not valid UTF-8".into()))?;
        let context =
            WhisperContext::new_with_params(path, WhisperContextParameters::default())
                .map_err(|e| SpeechError::Inference(format!("load model: {e}")))?;
        Ok(Self { context })
    }
}

#[async_trait]
impl Transcriber for LocalTranscriber {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResult, SpeechError> {
        // whisper.cpp expects mono f32 PCM at 16 kHz; platform::audio already
        // captures at that rate (CapturedAudio::sample_rate == 16_000).
        if request.sample_rate != 16_000 {
            return Err(SpeechError::Config(format!(
                "expected 16 kHz audio, got {}",
                request.sample_rate
            )));
        }

        let mut state = self
            .context
            .create_state()
            .map_err(|e| SpeechError::Inference(format!("create state: {e}")))?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        if let Some(lang) = request.language.as_deref() {
            params.set_language(Some(lang));
        }

        state
            .full(params, &request.samples)
            .map_err(|e| SpeechError::Inference(format!("inference: {e}")))?;

        let num_segments = state
            .full_n_segments()
            .map_err(|e| SpeechError::Inference(format!("segments: {e}")))?;
        let mut text = String::new();
        for i in 0..num_segments {
            let segment = state
                .full_get_segment_text(i)
                .map_err(|e| SpeechError::Inference(format!("segment text: {e}")))?;
            text.push_str(&segment);
        }

        Ok(TranscriptionResult {
            text: text.trim().to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_model_is_config_error() {
        let err = LocalTranscriber::load(PathBuf::from("/no/such/model.bin")).unwrap_err();
        assert!(matches!(err, SpeechError::Config(_)));
    }
}
```

- [ ] **Step 2: Run the test to verify it fails, then passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-infer local_transcriber`
Expected: the crate compiles (linking whisper.cpp via `whisper-rs`) and
`load_missing_model_is_config_error` PASSES. The first build compiles
whisper.cpp from source and may take several minutes — this is expected.

- [ ] **Step 3: Build the whole crate (CPU baseline)**

Run: `cargo build --manifest-path app/Cargo.toml -p vox-infer`
Expected: builds with default features (`cpu`). No GPU SDK is required.

- [ ] **Step 4: Build a GPU variant for the current OS (compile check only)**

Run the line matching your OS (skip if its SDK is not installed):
```bash
# macOS:
cargo build --manifest-path app/Cargo.toml -p vox-infer --no-default-features --features metal
# Linux/Windows with Vulkan SDK:
cargo build --manifest-path app/Cargo.toml -p vox-infer --no-default-features --features vulkan
# Linux/Windows with CUDA toolkit:
cargo build --manifest-path app/Cargo.toml -p vox-infer --no-default-features --features cuda
```
Expected: the selected GPU feature compiles. This only verifies the feature
wiring; correctness is the manual smoke check in Step 5.

- [ ] **Step 5: Manual inference smoke check (not a committed gate)**

Download a tiny model and transcribe a 16 kHz mono WAV to confirm end-to-end
inference. Run (adjust paths):
```bash
mkdir -p /tmp/vox-models
curl -L -o /tmp/vox-models/ggml-tiny.bin \
  https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin
```
Then, in a throwaway `examples/smoke.rs` or a scratch test, load
`LocalTranscriber::load("/tmp/vox-models/ggml-tiny.bin".into())` and call
`transcribe` on samples decoded from a known WAV; confirm the returned `text`
matches the spoken words. Do not commit the scratch file or the model.

- [ ] **Step 6: Commit**

```bash
git add app/crates/infer/src/local_transcriber.rs
git commit -m "feat(infer): add LocalTranscriber implementing Transcriber via whisper.cpp"
```

---

## Task 5: Extend Settings with `local_model_id`

**Files:**
- Modify: `app/crates/core/src/settings.rs`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `app/crates/core/src/settings.rs`:
```rust
    #[test]
    fn local_model_id_defaults_to_base() {
        let s = Settings::default();
        assert_eq!(s.local_model_id, "base");
    }

    #[test]
    fn local_model_id_missing_falls_back_to_default() {
        // An older settings file without the Phase 3 field still loads.
        let parsed: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed.local_model_id, "base");
    }

    #[test]
    fn schema_version_is_phase_three() {
        assert_eq!(Settings::default().schema_version, 4);
    }
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core settings`
Expected: FAIL — `local_model_id` does not exist and `schema_version` is still
the Phase 2 value (3).

- [ ] **Step 3: Add the field and bump the schema version**

In `Settings`, add the field (placed after the Phase 2 speech fields), and bump
`schema_version`. The struct already carries `#[serde(default)]`, so older files
without the field load fine. Add to the struct definition:
```rust
    pub local_model_id: String,
```
And in `impl Default for Settings`, bump the version and default the field
(shown here in context with the Phase 0–2 fields so the merge is unambiguous):
```rust
impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 4,
            rewrite_hotkey: "CmdOrCtrl+Shift+R".to_string(),
            speech_hotkey: "CmdOrCtrl+Shift+S".to_string(),
            launch_at_login: false,
            openai_base_url: "https://api.openai.com/v1".to_string(),
            rewrite_model: "gpt-4o-mini".to_string(),
            presets: crate::rewrite::PromptCatalog::defaults(),
            active_preset_id: "default".to_string(),
            speech_engine: crate::speech::SpeechEngineKind::OpenAi,
            speech_model: "gpt-4o-transcribe".to_string(),
            speech_language: None,
            local_model_id: "base".to_string(),
        }
    }
}
```
(If the Phase 1/2 default literals differ slightly in the live file, keep those
and add only the `schema_version: 4` bump and the `local_model_id` line.)

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core settings`
Expected: all settings tests PASS, including the three new Phase 3 assertions.

- [ ] **Step 5: Commit**

```bash
git add app/crates/core/src/settings.rs
git commit -m "feat(core): add local_model_id setting; bump schema_version to 4"
```

---

## Task 6: Engine factory (Local vs Remote) in src-tauri

**Files:**
- Modify: `app/src-tauri/Cargo.toml`
- Create: `app/src-tauri/src/engine.rs`
- Modify: `app/src-tauri/src/main.rs` (add `mod engine;`)
- Test: in-file `#[cfg(test)]` module in `engine.rs`

- [ ] **Step 1: Depend on `vox-infer` and forward GPU features**

Edit `app/src-tauri/Cargo.toml`. Add the dependency and a feature block that
forwards the binary's GPU features into `vox-infer` (so `tauri build --features
metal` selects the right backend). `tokio` is already present from Phase 2:
```toml
[dependencies]
vox-infer = { workspace = true }

[features]
# Default to the guaranteed CPU baseline.
default = ["cpu"]
cpu = ["vox-infer/cpu"]
metal = ["vox-infer/metal"]
cuda = ["vox-infer/cuda"]
vulkan = ["vox-infer/vulkan"]
```

- [ ] **Step 2: Write the failing test for the factory**

Create `app/src-tauri/src/engine.rs`. The factory returns a `Box<dyn
Transcriber>`; the test checks selection logic without doing inference: for
`WhisperLocal` with a model present it must return Ok (building a
`LocalTranscriber`), and for `WhisperLocal` with the model **absent** it must
surface the load error so the caller can prompt a download.
```rust
use vox_core::settings::Settings;
use vox_core::speech::{RemoteTranscriber, SpeechEngineKind, SpeechError, Transcriber};
use vox_infer::local_transcriber::LocalTranscriber;
use vox_infer::model_manager::ModelManager;

/// Build the active transcription engine from settings. Lives in `src-tauri`
/// because it depends on both `vox-core` and `vox-infer`.
pub fn build_transcriber(
    settings: &Settings,
    api_key: String,
    models: &ModelManager,
) -> Result<Box<dyn Transcriber>, SpeechError> {
    match settings.speech_engine {
        SpeechEngineKind::WhisperLocal => {
            let path = models.path_for(&settings.local_model_id);
            let local = LocalTranscriber::load(path)?;
            Ok(Box::new(local))
        }
        SpeechEngineKind::OpenAi | SpeechEngineKind::OpenAiCompatible => {
            Ok(Box::new(RemoteTranscriber::new(
                api_key,
                settings.openai_base_url.clone(),
                settings.speech_model.clone(),
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_engine_builds_for_openai() {
        let mut settings = Settings::default();
        settings.speech_engine = SpeechEngineKind::OpenAi;
        let dir = tempfile::tempdir().unwrap();
        let models = ModelManager::new(dir.path().to_path_buf());
        let engine = build_transcriber(&settings, "sk-test".into(), &models);
        assert!(engine.is_ok());
    }

    #[test]
    fn local_engine_errors_when_model_absent() {
        let mut settings = Settings::default();
        settings.speech_engine = SpeechEngineKind::WhisperLocal;
        settings.local_model_id = "base".into();
        let dir = tempfile::tempdir().unwrap(); // empty: no model on disk
        let models = ModelManager::new(dir.path().to_path_buf());
        let err = build_transcriber(&settings, "sk-test".into(), &models).unwrap_err();
        assert!(matches!(err, SpeechError::Config(_)));
    }

    #[test]
    fn local_engine_loads_attempted_when_file_present() {
        // A non-GGML file makes load() fail at the whisper layer (Inference),
        // proving the factory routed to LocalTranscriber rather than Remote.
        let mut settings = Settings::default();
        settings.speech_engine = SpeechEngineKind::WhisperLocal;
        settings.local_model_id = "base".into();
        let dir = tempfile::tempdir().unwrap();
        let models = ModelManager::new(dir.path().to_path_buf());
        std::fs::write(models.path_for("base"), b"not-a-real-ggml-model").unwrap();
        let err = build_transcriber(&settings, "sk-test".into(), &models).unwrap_err();
        assert!(matches!(err, SpeechError::Inference(_)));
    }
}
```
Add `tempfile` as a dev-dependency for `src-tauri` if not already present
(`app/src-tauri/Cargo.toml`):
```toml
[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Register the module**

Edit `app/src-tauri/src/main.rs` and add near the other module declarations:
```rust
mod engine;
```

- [ ] **Step 4: Run the test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox engine`
Expected: all three engine tests PASS — Remote builds for `OpenAi`, `WhisperLocal`
errors (`Config`) when the model is missing, and routes to `LocalTranscriber`
(error `Inference`) when a bad file is present.

- [ ] **Step 5: Commit**

```bash
git add app/src-tauri/Cargo.toml app/src-tauri/src/engine.rs app/src-tauri/src/main.rs
git commit -m "feat(app): engine factory selecting LocalTranscriber vs RemoteTranscriber"
```

---

## Task 7: `list_models` + `download_model` Tauri commands

**Files:**
- Modify: `app/src-tauri/src/commands.rs`
- Modify: `app/src-tauri/src/main.rs` (register the two commands)

- [ ] **Step 1: Write the model-listing DTO + commands**

Append to `app/src-tauri/src/commands.rs`. `list_models` returns each registry
entry plus whether it is on disk and whether it is the active model;
`download_model` streams the download and emits `model:progress` events. The
models directory is resolved next to the settings dir for consistency with
Phase 0's path strategy:
```rust
use std::path::PathBuf;

use serde::Serialize;
use tauri::{Emitter, Window};
use vox_core::settings::{default_settings_path, SettingsStore};
use vox_infer::model_manager::ModelManager;

/// Per-OS models directory (sibling of the settings file's directory).
fn models_dir() -> PathBuf {
    default_settings_path()
        .parent()
        .map(|p| p.join("models"))
        .unwrap_or_else(|| PathBuf::from("models"))
}

fn model_manager() -> ModelManager {
    ModelManager::new(models_dir())
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelEntry {
    pub id: String,
    pub display_name: String,
    pub size_bytes: u64,
    pub downloaded: bool,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ModelProgress {
    id: String,
    fraction: f64,
}

#[tauri::command]
pub fn list_models() -> Result<Vec<ModelEntry>, String> {
    let mgr = model_manager();
    let active = SettingsStore::at(default_settings_path())
        .load()
        .map_err(|e| e.to_string())?
        .local_model_id;
    Ok(ModelManager::registry()
        .into_iter()
        .map(|m| ModelEntry {
            downloaded: mgr.is_downloaded(&m.id),
            active: m.id == active,
            id: m.id,
            display_name: m.display_name,
            size_bytes: m.size_bytes,
        })
        .collect())
}

#[tauri::command]
pub async fn download_model(window: Window, model_id: String) -> Result<(), String> {
    let mgr = model_manager();
    let emit_id = model_id.clone();
    mgr.download(&model_id, move |fraction| {
        let _ = window.emit(
            "model:progress",
            ModelProgress {
                id: emit_id.clone(),
                fraction,
            },
        );
    })
    .await
    .map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Register the commands**

Edit `app/src-tauri/src/main.rs` and add the two commands to the existing
`tauri::generate_handler!` list (alongside the Phase 0–2 commands):
```rust
            commands::list_models,
            commands::download_model,
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo build --manifest-path app/Cargo.toml -p vox`
Expected: builds; `list_models` and `download_model` are registered. (Tauri's
`Emitter` trait is in scope for `window.emit`; it ships with Tauri 2.)

- [ ] **Step 4: Manual smoke check (not a committed gate)**

Run: `cd app && npm --prefix ui run tauri dev`, open the dev console, and run:
```js
await window.__TAURI__.core.invoke("list_models");
```
Expected: an array of model entries with `downloaded`/`active` flags; invoking
`download_model` with `{ modelId: "tiny" }` streams `model:progress` events.

- [ ] **Step 5: Commit**

```bash
git add app/src-tauri/src/commands.rs app/src-tauri/src/main.rs
git commit -m "feat(app): add list_models/download_model commands emitting model:progress"
```

---

## Task 8: Model-management UI

**Files:**
- Create: `app/ui/src/ModelManager.tsx`
- Modify: `app/ui/src/App.tsx`

- [ ] **Step 1: Build the model-management component**

Create `app/ui/src/ModelManager.tsx`. It lists registry models, shows
downloaded/active state, downloads with a live progress bar (driven by the
`model:progress` event), and sets the active model by saving `local_model_id`
through the existing settings commands:
```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type ModelEntry = {
  id: string;
  display_name: string;
  size_bytes: number;
  downloaded: boolean;
  active: boolean;
};

type ModelProgress = { id: string; fraction: number };

type Settings = {
  schema_version: number;
  local_model_id: string;
  [key: string]: unknown;
};

function formatSize(bytes: number): string {
  const gb = bytes / 1_000_000_000;
  if (gb >= 1) return `${gb.toFixed(1)} GB`;
  return `${Math.round(bytes / 1_000_000)} MB`;
}

export default function ModelManagerView() {
  const [models, setModels] = useState<ModelEntry[]>([]);
  const [progress, setProgress] = useState<Record<string, number>>({});
  const [status, setStatus] = useState("");

  async function refresh() {
    try {
      setModels(await invoke<ModelEntry[]>("list_models"));
    } catch (e) {
      setStatus(String(e));
    }
  }

  useEffect(() => {
    refresh();
    let unlisten: UnlistenFn | undefined;
    listen<ModelProgress>("model:progress", (event) => {
      setProgress((prev) => ({ ...prev, [event.payload.id]: event.payload.fraction }));
    }).then((fn) => {
      unlisten = fn;
    });
    return () => {
      if (unlisten) unlisten();
    };
  }, []);

  async function download(id: string) {
    setStatus("");
    setProgress((prev) => ({ ...prev, [id]: 0 }));
    try {
      await invoke("download_model", { modelId: id });
      await refresh();
    } catch (e) {
      setStatus(`Download failed: ${e}`);
    }
  }

  async function selectActive(id: string) {
    try {
      const settings = await invoke<Settings>("load_settings");
      await invoke("save_settings", { settings: { ...settings, local_model_id: id } });
      await refresh();
    } catch (e) {
      setStatus(String(e));
    }
  }

  return (
    <section style={{ marginTop: 24 }}>
      <h2>Offline models (whisper.cpp)</h2>
      <p style={{ color: "#666" }}>
        Download a model to enable offline dictation. Larger models are more
        accurate but slower and need more disk space.
      </p>
      <ul style={{ listStyle: "none", padding: 0 }}>
        {models.map((m) => {
          const frac = progress[m.id];
          const downloading = frac !== undefined && frac < 1 && !m.downloaded;
          return (
            <li
              key={m.id}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 12,
                padding: "8px 0",
                borderBottom: "1px solid #eee",
              }}
            >
              <span style={{ flex: 1 }}>
                <strong>{m.display_name}</strong>{" "}
                <small style={{ color: "#888" }}>{formatSize(m.size_bytes)}</small>
                {m.active && <em style={{ marginLeft: 8, color: "#2563eb" }}>active</em>}
              </span>
              {downloading ? (
                <span style={{ width: 160 }}>
                  <progress value={frac} max={1} style={{ width: "100%" }} />
                  <small style={{ marginLeft: 6 }}>{Math.round(frac * 100)}%</small>
                </span>
              ) : m.downloaded ? (
                <button disabled={m.active} onClick={() => selectActive(m.id)}>
                  {m.active ? "Selected" : "Use this model"}
                </button>
              ) : (
                <button onClick={() => download(m.id)}>Download</button>
              )}
            </li>
          );
        })}
      </ul>
      {status && <p style={{ color: "crimson" }}>{status}</p>}
    </section>
  );
}
```

- [ ] **Step 2: Mount it in the settings page**

Edit `app/ui/src/App.tsx` to render the model manager below the existing
settings form. Add the import at the top:
```tsx
import ModelManagerView from "./ModelManager";
```
And render it inside the returned `<main>`, after the existing Save button/status
(before the closing `</main>`):
```tsx
      <ModelManagerView />
```

- [ ] **Step 3: Verify the UI builds**

Run: `cd app && npm --prefix ui run build`
Expected: Vite build succeeds; `ui/dist` is produced with the new component.

- [ ] **Step 4: Manual smoke check (not a committed gate)**

Run: `cd app && npm --prefix ui run tauri dev`
Expected: the settings window lists models; "Download" on `tiny` shows a live
progress bar that fills to 100%, then offers "Use this model"; selecting it marks
it `active` and persists `local_model_id`.

- [ ] **Step 5: Commit**

```bash
git add app/ui/src/ModelManager.tsx app/ui/src/App.tsx
git commit -m "feat(ui): add offline model management (list, download, select)"
```

---

## Task 9: CI — CPU baseline everywhere + per-OS GPU builds

**Files:**
- Modify: `.github/workflows/app-ci.yml`

- [ ] **Step 1: Add the `vox-infer` CPU baseline test to the existing build job**

Edit `.github/workflows/app-ci.yml`. The whisper.cpp build needs CMake, which
the GitHub runners already provide; add a `vox-infer` test step to the existing
matrix `build` job (after the existing `vox-core` test step):
```yaml
      - name: Rust unit tests (infer, CPU baseline)
        run: cargo test --manifest-path app/Cargo.toml -p vox-infer
      - name: Rust unit tests (src-tauri engine factory)
        run: cargo test --manifest-path app/Cargo.toml -p vox engine
```

- [ ] **Step 2: Add a separate per-OS GPU build job**

Append a new job to the same workflow file. It compiles the GPU feature on the
matching OS (Metal on macOS, Vulkan on Linux/Windows where an SDK is available)
to keep the feature wiring honest; it does not run inference. CPU stays the
guaranteed baseline (covered by the `build` job above):
```yaml
  gpu-build:
    name: GPU feature build (${{ matrix.os }})
    needs: build
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: macos-latest
            feature: metal
          - os: ubuntu-latest
            feature: vulkan
          - os: windows-latest
            feature: vulkan
    runs-on: ${{ matrix.os }}
    timeout-minutes: 60
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - name: Install Linux build + Vulkan deps
        if: matrix.os == 'ubuntu-latest'
        run: |
          sudo apt-get update
          sudo apt-get install -y cmake build-essential libwebkit2gtk-4.1-dev \
            libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev \
            libvulkan-dev glslc vulkan-tools
      - name: Install Vulkan SDK (Windows)
        if: matrix.os == 'windows-latest'
        uses: humbletim/install-vulkan-sdk@v1.2
        with:
          version: latest
          cache: true
      - name: Build vox-infer with ${{ matrix.feature }}
        run: |
          cargo build --manifest-path app/Cargo.toml -p vox-infer \
            --no-default-features --features ${{ matrix.feature }}
```

- [ ] **Step 3: Verify locally where possible**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-infer && cargo test --manifest-path app/Cargo.toml -p vox engine`
Expected: the CPU-baseline infer tests and the engine-factory tests pass on the
current OS (mirrors the new CI steps).

- [ ] **Step 4: Commit and push to trigger CI**

```bash
git add .github/workflows/app-ci.yml
git commit -m "ci: test vox-infer CPU baseline on all OSes; build GPU variants per OS"
git push
```
Expected: **App CI** runs; the `build` matrix passes the CPU baseline on all
three OSes and the `gpu-build` job compiles Metal (macOS) and Vulkan
(Linux/Windows). Verify with `gh run list --workflow=app-ci.yml`.

---

## Phase 3 Acceptance

- `cargo test --manifest-path app/Cargo.toml -p vox-infer` passes (registry,
  path/`is_downloaded`, and streaming-download-with-progress unit tests).
- `cargo test --manifest-path app/Cargo.toml -p vox-core settings` passes,
  including the `local_model_id` default and `schema_version == 4` checks.
- `cargo test --manifest-path app/Cargo.toml -p vox engine` passes (factory
  routes `WhisperLocal` → `LocalTranscriber`, others → `RemoteTranscriber`).
- `cargo build --manifest-path app/Cargo.toml -p vox-infer` (CPU baseline) and
  the matching GPU feature (`metal`/`vulkan`/`cuda`) build for the host OS.
- **App CI** is green: CPU baseline tested on macOS/Windows/Linux and GPU
  variants built per OS.
- Manual: in `tauri dev`, the model UI lists `tiny..large-v3`, downloads a model
  with a live progress bar, and lets the user select the active model; with a
  downloaded model and `speech_engine = WhisperLocal`, push-to-talk dictation
  transcribes **offline** (no network) on all three OSes.

## Self-Review Notes

- **Spec coverage:** Implements the spec's Phase 3 scope and Decision D1 —
  `vox-infer` added to the workspace, `LocalTranscriber` over whisper.cpp
  (`whisper-rs`), model registry/download/storage + management UI (replacing the
  WhisperKit download manager), GPU acceleration via feature flags (Metal /
  CUDA / Vulkan with a guaranteed CPU baseline), and the engine factory wiring
  `SpeechEngineKind::WhisperLocal` into the existing Phase 2 dictation pipeline.
  Parakeet is intentionally absent (D1). Streaming partial transcription remains
  a non-goal (full-utterance transcription only).
- **Type consistency vs. CONTRACTS.md:** `vox-infer`, modules `model_manager` /
  `local_transcriber`, and `lib.rs` re-exports match the contracts layout.
  `WhisperModel { id, display_name, url, size_bytes }`, `ModelManager::{new,
  registry, is_downloaded, path_for, download}` with `download(&self, model_id,
  on_progress: impl Fn(f64) + Send) -> Result<(), SpeechError>`, and
  `LocalTranscriber::load(model_path: PathBuf) -> Result<Self, SpeechError>`
  implementing `core::speech::Transcriber` all match verbatim. The factory uses
  `SpeechEngineKind`, `RemoteTranscriber::new(api_key, base_url, model)`,
  `TranscriptionRequest/Result`, and `SpeechError` from Phase 2 unchanged; the
  Phase 3 setting is `local_model_id: String` defaulting to `"base"` with
  `schema_version` bumped (to 4) per the additive-settings convention. Commands
  `list_models`/`download_model` and the `model:progress` event match the Tauri
  naming table. `download_from` is a within-phase test seam (not a contract
  type) that the public, contract `download` delegates to.
- **No placeholders:** every code/command step is complete, compilable
  Rust/TS/YAML with no `TBD`, no "similar to above", and no elided bodies. The
  only deliberately manual steps are the GPU inference smoke check (Task 4
  Step 5) and the `tauri dev` UI checks, each explicitly marked "not a committed
  gate" and excluded from CI because they require a downloaded model / GPU SDK.
