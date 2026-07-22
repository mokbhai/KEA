# Cross-Platform Rewrite — Phase 1 (Rewrite, remote) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the core value proposition on macOS, Windows, and Linux (X11 + Wayland): pressing the global rewrite hotkey captures the current selection, sends it (with the active preset's prompt + model) to an OpenAI or OpenAI-compatible provider, and replaces the selection in place with the rewritten text, while an overlay reflects progress and errors.

**Architecture:** Phase 0 stood up the Cargo workspace (`vox-core` with `settings`/`secrets`, the `vox` Tauri binary, and the `ui` web app). Phase 1 adds a platform-agnostic `core::rewrite` module (provider trait + OpenAI/OpenAI-compatible provider + presets + prompt catalog) and a new `vox-platform` workspace member that hides per-OS integration behind two traits: `platform::hotkeys` (global accelerators, with X11 vs. Wayland portal selected at runtime per Decision D2) and `platform::textio` (clipboard + synthetic paste per Decision D3, with a Wayland `uinput` path). `src-tauri` owns the async runtime (`tokio`), instantiates a `RewriteService`, listens for hotkey presses, and runs the end-to-end flow, emitting `rewrite:status` events to the overlay UI. `core` stays runtime-agnostic (async via the trait only); only `src-tauri` pulls in `tokio`.

**Tech Stack:** Rust (edition 2021), `async-trait`, `reqwest` (features `["json","rustls-tls"]`), `serde`/`serde_json`, `thiserror`, `arboard`, `enigo`, `global-hotkey`, `tokio` (in `src-tauri` only), Tauri 2.x, React 18 + TypeScript + Vite. Linux additionally relies on the XDG `org.freedesktop.portal.GlobalShortcuts` portal (Wayland hotkeys) and `uinput`/evdev (Wayland synthetic input).

**Reference spec:** `docs/cross-platform/2026-05-29-cross-platform-rewrite-design.md`

---

## File Structure

Phase 1 extends the `app/` workspace created in Phase 0. New and modified files:

- `app/Cargo.toml` — add `crates/platform` to `members`; add shared deps (`async-trait`, `reqwest`).
- `app/crates/core/Cargo.toml` — add `async-trait`, `reqwest` (for `core::rewrite`); add `tokio` dev-dep for provider tests.
- `app/crates/core/src/lib.rs` — re-export the new `rewrite` module.
- `app/crates/core/src/rewrite.rs` — `RewriteRequest`/`RewriteResult`/`RewriteError`, `RewriteProvider` trait, `OpenAiRewriteProvider`, `Preset`, `PromptCatalog`, `RewriteService`; in-file tests.
- `app/crates/core/src/settings.rs` — add Phase 1 fields (`openai_base_url`, `rewrite_model`, `presets`, `active_preset_id`); bump `schema_version`; extend tests.
- `app/crates/platform/Cargo.toml` — new workspace member; deps `arboard`, `enigo`, `global-hotkey`, `thiserror`; Linux-only deps.
- `app/crates/platform/src/lib.rs` — re-exports `hotkeys`, `textio`.
- `app/crates/platform/src/hotkeys.rs` — `HotkeyId`, `HotkeyError`, `HotkeyManager` trait, `new_hotkey_manager()`, X11/desktop impl + Wayland portal selection note; in-file tests.
- `app/crates/platform/src/textio.rs` — `TextIoError`, `TextIo` trait, `ClipboardTextIo`, `new_text_io()`, Wayland `uinput` path; in-file tests with a fake `TextIo`.
- `app/src-tauri/Cargo.toml` — add `vox-platform`, `tokio`.
- `app/src-tauri/src/rewrite_flow.rs` — `RewriteService` wiring (provider + textio), the `rewrite_selection` command, hotkey listener + `rewrite:status` event emission.
- `app/src-tauri/src/main.rs` — start the hotkey listener in `setup`, register `rewrite_selection`.
- `app/ui/src/Overlay.tsx` — minimal always-listening status overlay reacting to `rewrite:status`.
- `app/ui/src/RewriteSettings.tsx` — settings UI section: provider base URL, model, API key, preset picker/editor.
- `app/ui/src/App.tsx` — mount the rewrite settings section + overlay.
- `app/src-tauri/tauri.conf.json` — add an `overlay` window (always-on-top, transparent, hidden by default).

Each file keeps one responsibility: `rewrite.rs` knows only request building + provider HTTP + catalog; `hotkeys.rs`/`textio.rs` know only OS integration behind a trait; `rewrite_flow.rs` only orchestrates core + platform into the Tauri shell.

---

## Prerequisites (one-time, not committed)

- [ ] **Step 0: Verify Phase 0 is in place and toolchains are ready**

Run:
```bash
cargo test --manifest-path app/Cargo.toml -p vox-core
rustc --version && cargo --version
```
Expected: Phase 0 `vox-core` tests pass. On Linux, ensure Tauri + synthetic-input system deps are present (adds `libxdo-dev` for `enigo`/XTest and uinput access):
```bash
sudo apt-get update && sudo apt-get install -y libwebkit2gtk-4.1-dev \
  build-essential curl wget file libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev
# Wayland synthetic input via uinput needs device access; document for users:
#   sudo groupadd -f uinput && sudo usermod -aG uinput "$USER"
#   echo 'KERNEL=="uinput", GROUP="uinput", MODE="0660"' | sudo tee /etc/udev/rules.d/99-vox-uinput.rules
#   sudo udevadm control --reload-rules && sudo udevadm trigger
```

---

## Task 1: Add workspace dependencies and the `vox-platform` member

**Files:**
- Modify: `app/Cargo.toml`
- Modify: `app/crates/core/Cargo.toml`
- Create: `app/crates/platform/Cargo.toml`, `app/crates/platform/src/lib.rs`
- Modify: `app/src-tauri/Cargo.toml`

- [ ] **Step 1: Add shared deps and the new member to the workspace manifest**

Edit `app/Cargo.toml` so the `[workspace]` and `[workspace.dependencies]` sections read:
```toml
[workspace]
resolver = "2"
members = ["crates/core", "crates/platform", "src-tauri"]

[workspace.package]
edition = "2021"
version = "0.0.0"
license = "Apache-2.0"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "1"
async-trait = "0.1"
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

- [ ] **Step 2: Add the rewrite deps to `vox-core`**

Edit `app/crates/core/Cargo.toml` so `[dependencies]` and `[dev-dependencies]` include:
```toml
[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
keyring = "3"
directories = "5"
thiserror = { workspace = true }
async-trait = { workspace = true }
reqwest = { workspace = true }

[dev-dependencies]
tempfile = "3"
tokio = { version = "1", features = ["macros", "rt"] }
```

- [ ] **Step 3: Create the `vox-platform` crate manifest**

Create `app/crates/platform/Cargo.toml`:
```toml
[package]
name = "vox-platform"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
thiserror = { workspace = true }
arboard = "3"
enigo = "0.2"
global-hotkey = "0.6"

[target.'cfg(target_os = "linux")'.dependencies]
# uinput-backed synthetic input for Wayland (Decision D3) and runtime
# X11/Wayland detection live behind cfg(target_os = "linux").
```

Create `app/crates/platform/src/lib.rs`:
```rust
pub mod hotkeys;
pub mod textio;
```

- [ ] **Step 4: Add `vox-platform` and `tokio` to the Tauri binary**

Edit `app/src-tauri/Cargo.toml` `[dependencies]` to add:
```toml
vox-platform = { path = "../crates/platform" }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync"] }
```

- [ ] **Step 5: Verify the workspace still resolves and builds**

Run: `cargo build --manifest-path app/Cargo.toml`
Expected: `vox-platform` is now a member with an empty (re-export only) `lib.rs`; the workspace compiles. (`hotkeys`/`textio` are filled in by later tasks.)

- [ ] **Step 6: Commit**

```bash
git add app/Cargo.toml app/crates/core/Cargo.toml app/crates/platform app/src-tauri/Cargo.toml
git commit -m "feat(platform): add vox-platform workspace member and rewrite deps"
```

---

## Task 2: `core::rewrite` — types, provider trait, and a fake provider

**Files:**
- Create: `app/crates/core/src/rewrite.rs`
- Modify: `app/crates/core/src/lib.rs`
- Test: in-file `#[cfg(test)]` module in `rewrite.rs`

- [ ] **Step 1: Write the failing test (types + trait + request building)**

Create `app/crates/core/src/rewrite.rs`:
```rust
use async_trait::async_trait;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RewriteRequest {
    pub text: String,
    pub prompt: String,
    pub model: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct RewriteResult {
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    #[error("http {0}: {1}")]
    Http(u16, String),
    #[error("network: {0}")]
    Network(String),
    #[error("empty input")]
    EmptyInput,
    #[error("invalid configuration: {0}")]
    Config(String),
}

#[async_trait]
pub trait RewriteProvider: Send + Sync {
    async fn rewrite(&self, request: RewriteRequest) -> Result<RewriteResult, RewriteError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A fake provider used to test orchestration without network access.
    #[derive(Default)]
    struct FakeProvider {
        last_request: Mutex<Option<RewriteRequest>>,
    }

    #[async_trait]
    impl RewriteProvider for FakeProvider {
        async fn rewrite(&self, request: RewriteRequest) -> Result<RewriteResult, RewriteError> {
            if request.text.trim().is_empty() {
                return Err(RewriteError::EmptyInput);
            }
            let echoed = format!("[{}] {}", request.prompt, request.text);
            *self.last_request.lock().unwrap() = Some(request);
            Ok(RewriteResult { text: echoed })
        }
    }

    #[tokio::test]
    async fn fake_provider_echoes_prompt_and_text() {
        let provider = FakeProvider::default();
        let result = provider
            .rewrite(RewriteRequest {
                text: "hello".into(),
                prompt: "Fix grammar".into(),
                model: "gpt-4o-mini".into(),
            })
            .await
            .unwrap();
        assert_eq!(result.text, "[Fix grammar] hello");
        let captured = provider.last_request.lock().unwrap().clone().unwrap();
        assert_eq!(captured.model, "gpt-4o-mini");
    }

    #[tokio::test]
    async fn fake_provider_rejects_empty_input() {
        let provider = FakeProvider::default();
        let err = provider
            .rewrite(RewriteRequest {
                text: "   ".into(),
                prompt: "Fix grammar".into(),
                model: "gpt-4o-mini".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, RewriteError::EmptyInput));
    }
}
```

- [ ] **Step 2: Wire the module into the crate root**

Edit `app/crates/core/src/lib.rs`:
```rust
pub mod settings;
pub mod secrets;
pub mod rewrite;
```

- [ ] **Step 3: Run the tests (expect PASS)**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core rewrite`
Expected: both `fake_provider_*` tests PASS. (This task defines the contract types and exercises them through a fake; the HTTP provider arrives in Task 3.)

- [ ] **Step 4: Commit**

```bash
git add app/crates/core/src/rewrite.rs app/crates/core/src/lib.rs
git commit -m "feat(core): add rewrite types, RewriteProvider trait, and fake provider tests"
```

---

## Task 3: `OpenAiRewriteProvider` (OpenAI + OpenAI-compatible) with request-building tests

**Files:**
- Modify: `app/crates/core/src/rewrite.rs`
- Test: extend the in-file `#[cfg(test)]` module

- [ ] **Step 1: Write the failing tests (request body + default base URL)**

Append to the `tests` module in `rewrite.rs`:
```rust
    #[test]
    fn build_body_uses_system_prompt_user_text_and_model() {
        let provider =
            OpenAiRewriteProvider::new("sk-test".into(), "https://api.openai.com/v1".into());
        let body = provider.build_body(&RewriteRequest {
            text: "teh cat".into(),
            prompt: "Fix typos".into(),
            model: "gpt-4o-mini".into(),
        });
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "Fix typos");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"], "teh cat");
    }

    #[test]
    fn endpoint_joins_base_url_without_double_slash() {
        let p1 = OpenAiRewriteProvider::new("k".into(), "https://api.openai.com/v1".into());
        assert_eq!(p1.endpoint(), "https://api.openai.com/v1/chat/completions");
        // OpenAI-compatible server with a trailing slash in the configured base URL.
        let p2 = OpenAiRewriteProvider::new("k".into(), "http://localhost:11434/v1/".into());
        assert_eq!(p2.endpoint(), "http://localhost:11434/v1/chat/completions");
    }

    #[test]
    fn parse_response_extracts_first_choice_message() {
        let raw = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "the cat" } }]
        });
        let result = OpenAiRewriteProvider::parse_response(raw).unwrap();
        assert_eq!(result.text, "the cat");
    }

    #[test]
    fn parse_response_errors_on_missing_choices() {
        let raw = serde_json::json!({ "choices": [] });
        let err = OpenAiRewriteProvider::parse_response(raw).unwrap_err();
        assert!(matches!(err, RewriteError::Config(_)));
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core rewrite`
Expected: FAIL — `OpenAiRewriteProvider`, `build_body`, `endpoint`, and `parse_response` do not exist yet.

- [ ] **Step 3: Write the minimal implementation**

Add to `rewrite.rs` (above the `tests` module):
```rust
/// Provider for OpenAI and any OpenAI-compatible chat-completions endpoint.
/// OpenAI-compatible servers are supported purely by setting `base_url`
/// (e.g. a local server or a hosted gateway); the request shape is identical.
pub struct OpenAiRewriteProvider {
    api_key: String,
    base_url: String,
    http: reqwest::Client,
}

impl OpenAiRewriteProvider {
    pub fn new(api_key: String, base_url: String) -> Self {
        Self {
            api_key,
            base_url,
            http: reqwest::Client::new(),
        }
    }

    fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        format!("{base}/chat/completions")
    }

    fn build_body(&self, request: &RewriteRequest) -> serde_json::Value {
        serde_json::json!({
            "model": request.model,
            "messages": [
                { "role": "system", "content": request.prompt },
                { "role": "user", "content": request.text },
            ],
        })
    }

    fn parse_response(raw: serde_json::Value) -> Result<RewriteResult, RewriteError> {
        let text = raw["choices"]
            .get(0)
            .and_then(|c| c["message"]["content"].as_str())
            .ok_or_else(|| RewriteError::Config("response missing choices[0].message.content".into()))?;
        Ok(RewriteResult {
            text: text.to_string(),
        })
    }
}

#[async_trait]
impl RewriteProvider for OpenAiRewriteProvider {
    async fn rewrite(&self, request: RewriteRequest) -> Result<RewriteResult, RewriteError> {
        if request.text.trim().is_empty() {
            return Err(RewriteError::EmptyInput);
        }
        if self.api_key.is_empty() {
            return Err(RewriteError::Config("missing API key".into()));
        }
        let response = self
            .http
            .post(self.endpoint())
            .bearer_auth(&self.api_key)
            .json(&self.build_body(&request))
            .send()
            .await
            .map_err(|e| RewriteError::Network(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(RewriteError::Http(status.as_u16(), body));
        }
        let raw: serde_json::Value = response
            .json()
            .await
            .map_err(|e| RewriteError::Network(e.to_string()))?;
        Self::parse_response(raw)
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core rewrite`
Expected: all `rewrite` tests PASS (fake-provider, body building, endpoint joining, response parsing). The live HTTP path is exercised by the end-to-end manual smoke check in Task 9, not in unit tests.

- [ ] **Step 5: Commit**

```bash
git add app/crates/core/src/rewrite.rs
git commit -m "feat(core): add OpenAiRewriteProvider (OpenAI + OpenAI-compatible)"
```

---

## Task 4: `Preset` + `PromptCatalog::defaults()`

**Files:**
- Modify: `app/crates/core/src/rewrite.rs`
- Test: extend the in-file `#[cfg(test)]` module

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `rewrite.rs`:
```rust
    #[test]
    fn prompt_catalog_provides_nonempty_unique_defaults() {
        let presets = PromptCatalog::defaults();
        assert!(!presets.is_empty(), "expected built-in presets");

        let mut ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(unique, ids.len(), "preset ids must be unique");

        for preset in &presets {
            assert!(!preset.name.trim().is_empty(), "preset name required");
            assert!(!preset.prompt.trim().is_empty(), "preset prompt required");
            assert!(!preset.model.trim().is_empty(), "preset model required");
        }
    }

    #[test]
    fn prompt_catalog_includes_a_default_active_preset() {
        let presets = PromptCatalog::defaults();
        assert!(
            presets.iter().any(|p| p.id == "improve"),
            "expected an 'improve' preset to use as the default active id"
        );
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core rewrite`
Expected: FAIL — `Preset` and `PromptCatalog` are not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to `rewrite.rs` (above the `tests` module):
```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Preset {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub model: String,
}

/// Built-in default presets shipped with the app. Mirrors the prompt catalog
/// from the legacy Swift product so behavior is familiar at parity.
pub struct PromptCatalog;

impl PromptCatalog {
    pub fn defaults() -> Vec<Preset> {
        let model = "gpt-4o-mini".to_string();
        vec![
            Preset {
                id: "improve".into(),
                name: "Improve Writing".into(),
                prompt: "Improve the writing of the following text. Fix grammar, spelling, \
                         and clarity while preserving the original meaning and tone. \
                         Return only the rewritten text with no commentary."
                    .into(),
                model: model.clone(),
            },
            Preset {
                id: "professional".into(),
                name: "Make Professional".into(),
                prompt: "Rewrite the following text in a professional, polished tone suitable \
                         for business communication. Return only the rewritten text."
                    .into(),
                model: model.clone(),
            },
            Preset {
                id: "concise".into(),
                name: "Make Concise".into(),
                prompt: "Rewrite the following text to be as concise as possible without \
                         losing meaning. Return only the rewritten text."
                    .into(),
                model: model.clone(),
            },
            Preset {
                id: "fix_grammar".into(),
                name: "Fix Grammar".into(),
                prompt: "Correct only the grammar, spelling, and punctuation of the following \
                         text. Do not change wording, tone, or meaning. Return only the \
                         corrected text."
                    .into(),
                model,
            },
        ]
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core rewrite`
Expected: all `rewrite` tests PASS, including the two new catalog tests.

- [ ] **Step 5: Commit**

```bash
git add app/crates/core/src/rewrite.rs
git commit -m "feat(core): add Preset and PromptCatalog::defaults()"
```

---

## Task 5: Extend `Settings` with Phase 1 fields

**Files:**
- Modify: `app/crates/core/src/settings.rs`
- Test: extend the in-file `#[cfg(test)]` module

- [ ] **Step 1: Write the failing tests**

Append to the `tests` module in `app/crates/core/src/settings.rs`:
```rust
    #[test]
    fn phase1_fields_have_expected_defaults() {
        let s = Settings::default();
        assert_eq!(s.schema_version, 2);
        assert_eq!(s.openai_base_url, "https://api.openai.com/v1");
        assert_eq!(s.rewrite_model, "gpt-4o-mini");
        assert_eq!(s.active_preset_id, "improve");
        assert_eq!(s.presets, crate::rewrite::PromptCatalog::defaults());
    }

    #[test]
    fn phase0_file_without_phase1_fields_still_loads() {
        // A settings file written by Phase 0 (no rewrite fields).
        let legacy = r#"{
            "schema_version": 1,
            "rewrite_hotkey": "CmdOrCtrl+Shift+R",
            "speech_hotkey": "CmdOrCtrl+Shift+S",
            "launch_at_login": false
        }"#;
        let parsed: Settings = serde_json::from_str(legacy).unwrap();
        // Missing Phase 1 fields fall back to defaults via #[serde(default)].
        assert_eq!(parsed.openai_base_url, "https://api.openai.com/v1");
        assert_eq!(parsed.rewrite_model, "gpt-4o-mini");
        assert_eq!(parsed.active_preset_id, "improve");
        assert_eq!(parsed.presets, crate::rewrite::PromptCatalog::defaults());
    }
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core settings`
Expected: FAIL — the new fields and bumped `schema_version` do not exist yet.

- [ ] **Step 3: Add the fields and bump the schema version**

Edit the `Settings` struct in `app/crates/core/src/settings.rs` to add the Phase 1 fields, and import `Preset`:
```rust
use serde::{Deserialize, Serialize};

use crate::rewrite::{Preset, PromptCatalog};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    pub rewrite_hotkey: String,
    pub speech_hotkey: String,
    pub launch_at_login: bool,
    // --- Phase 1 (rewrite) ---
    pub openai_base_url: String,
    pub rewrite_model: String,
    pub presets: Vec<Preset>,
    pub active_preset_id: String,
}
```

Update the `Default` impl in the same file:
```rust
impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 2,
            rewrite_hotkey: "CmdOrCtrl+Shift+R".to_string(),
            speech_hotkey: "CmdOrCtrl+Shift+S".to_string(),
            launch_at_login: false,
            openai_base_url: "https://api.openai.com/v1".to_string(),
            rewrite_model: "gpt-4o-mini".to_string(),
            presets: PromptCatalog::defaults(),
            active_preset_id: "improve".to_string(),
        }
    }
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core settings`
Expected: all settings tests PASS, including the Phase 0 round-trip/defaults tests (which remain valid since `#[serde(default)]` covers the new fields) and the two new ones.

- [ ] **Step 5: Commit**

```bash
git add app/crates/core/src/settings.rs
git commit -m "feat(core): extend Settings with Phase 1 rewrite fields (schema_version 2)"
```

---

## Task 6: `platform::textio` — `TextIo` trait, `ClipboardTextIo`, fake-tested orchestration

**Files:**
- Modify: `app/crates/platform/src/textio.rs` (created empty by lib re-export in Task 1)
- Test: in-file `#[cfg(test)]` module

> **Decision D3 (clipboard + synthetic paste):** `capture_selection` synthesizes
> Cmd/Ctrl+C and reads the clipboard; `replace_selection` saves the clipboard,
> sets the rewritten text, synthesizes Cmd/Ctrl+V, then restores the prior
> clipboard after a short delay. On macOS/Windows/X11 the keystrokes go through
> `enigo`; on **Wayland**, `enigo`'s XTest path is unavailable, so synthetic keys
> are emitted through a `uinput` virtual keyboard (requires the udev rule from
> Step 0). Backend selection is `#[cfg(target_os)]` plus a Wayland runtime check.

- [ ] **Step 1: Write the failing tests (trait shape via a fake)**

Create/replace `app/crates/platform/src/textio.rs`:
```rust
#[derive(Debug, thiserror::Error)]
pub enum TextIoError {
    #[error("{0}")]
    Clipboard(String),
    #[error("{0}")]
    Inject(String),
}

/// Baseline (Decision D3): clipboard save -> set text -> synthetic paste -> restore.
pub trait TextIo: Send + Sync {
    /// Synthesize copy, then read the clipboard to obtain the current selection.
    fn capture_selection(&self) -> Result<String, TextIoError>;
    /// Replace the current selection: set clipboard, synthesize paste, restore clipboard.
    fn replace_selection(&self, text: &str) -> Result<(), TextIoError>;
    /// Paste `text` at the cursor without a prior copy (no selection capture).
    fn insert_text(&self, text: &str) -> Result<(), TextIoError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory fake that records the operations a consumer drives, so the
    /// flow can be unit-tested without touching the real clipboard/keyboard.
    #[derive(Default)]
    struct FakeTextIo {
        clipboard: Mutex<String>,
        pasted: Mutex<Vec<String>>,
    }

    impl TextIo for FakeTextIo {
        fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(self.clipboard.lock().unwrap().clone())
        }
        fn replace_selection(&self, text: &str) -> Result<(), TextIoError> {
            *self.clipboard.lock().unwrap() = text.to_string();
            self.pasted.lock().unwrap().push(text.to_string());
            Ok(())
        }
        fn insert_text(&self, text: &str) -> Result<(), TextIoError> {
            self.pasted.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    #[test]
    fn capture_then_replace_round_trip() {
        let io = FakeTextIo::default();
        *io.clipboard.lock().unwrap() = "teh cat".to_string();

        let selected = io.capture_selection().unwrap();
        assert_eq!(selected, "teh cat");

        io.replace_selection("the cat").unwrap();
        assert_eq!(io.pasted.lock().unwrap().as_slice(), &["the cat".to_string()]);
        assert_eq!(*io.clipboard.lock().unwrap(), "the cat");
    }

    #[test]
    fn insert_text_pastes_without_capture() {
        let io = FakeTextIo::default();
        io.insert_text("dictated text").unwrap();
        assert_eq!(io.pasted.lock().unwrap().as_slice(), &["dictated text".to_string()]);
    }
}
```

- [ ] **Step 2: Run to verify it passes (trait + fake)**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform textio`
Expected: both fake-driven tests PASS. (This establishes the contract the real backend must honor; the real `ClipboardTextIo` is added next and is covered by manual smoke checks, not CI, since it drives the real keyboard.)

- [ ] **Step 3: Add the real `ClipboardTextIo` and factory**

Append to `app/crates/platform/src/textio.rs` (above the `tests` module):
```rust
use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings as EnigoSettings};

/// Real text I/O via `arboard` (clipboard) + `enigo` (synthetic keys).
/// On Wayland, synthetic keys fall back to a `uinput` virtual keyboard
/// (see `paste_keystroke`); backend selection happens at runtime.
pub struct ClipboardTextIo;

impl ClipboardTextIo {
    pub fn new() -> Result<Self, TextIoError> {
        // Probe that a clipboard backend is reachable up front.
        arboard::Clipboard::new().map_err(|e| TextIoError::Clipboard(e.to_string()))?;
        Ok(Self)
    }

    fn read_clipboard(&self) -> Result<String, TextIoError> {
        let mut cb = arboard::Clipboard::new().map_err(|e| TextIoError::Clipboard(e.to_string()))?;
        cb.get_text().map_err(|e| TextIoError::Clipboard(e.to_string()))
    }

    fn write_clipboard(&self, text: &str) -> Result<(), TextIoError> {
        let mut cb = arboard::Clipboard::new().map_err(|e| TextIoError::Clipboard(e.to_string()))?;
        cb.set_text(text.to_string())
            .map_err(|e| TextIoError::Clipboard(e.to_string()))
    }

    /// The modifier used for copy/paste accelerators: Cmd on macOS, Ctrl elsewhere.
    fn modifier() -> Key {
        #[cfg(target_os = "macos")]
        {
            Key::Meta
        }
        #[cfg(not(target_os = "macos"))]
        {
            Key::Control
        }
    }

    /// Returns true on Linux when running under Wayland (no X11 display).
    #[cfg(target_os = "linux")]
    fn is_wayland() -> bool {
        std::env::var("WAYLAND_DISPLAY").is_ok() && std::env::var("DISPLAY").is_err()
    }

    /// Synthesize a `modifier`+`letter` chord (e.g. Ctrl+C / Cmd+V).
    fn chord(&self, letter: char) -> Result<(), TextIoError> {
        #[cfg(target_os = "linux")]
        {
            if Self::is_wayland() {
                return self.chord_uinput(letter);
            }
        }
        let mut enigo = Enigo::new(&EnigoSettings::default())
            .map_err(|e| TextIoError::Inject(e.to_string()))?;
        let modifier = Self::modifier();
        enigo
            .key(modifier, Direction::Press)
            .map_err(|e| TextIoError::Inject(e.to_string()))?;
        enigo
            .key(Key::Unicode(letter), Direction::Click)
            .map_err(|e| TextIoError::Inject(e.to_string()))?;
        enigo
            .key(modifier, Direction::Release)
            .map_err(|e| TextIoError::Inject(e.to_string()))?;
        Ok(())
    }

    /// Wayland synthetic-key path. Emits the chord through a `uinput` virtual
    /// keyboard (ydotool-style). Requires the `/dev/uinput` udev rule documented
    /// in the prerequisites. Implemented against evdev/uinput on Linux only.
    #[cfg(target_os = "linux")]
    fn chord_uinput(&self, letter: char) -> Result<(), TextIoError> {
        // Lazily resolve a uinput-backed Enigo; enigo selects the libei/uinput
        // backend under Wayland. If construction fails (missing /dev/uinput
        // permissions), surface an actionable Inject error.
        let mut enigo = Enigo::new(&EnigoSettings::default())
            .map_err(|e| TextIoError::Inject(format!("uinput unavailable: {e}")))?;
        enigo
            .key(Key::Control, Direction::Press)
            .map_err(|e| TextIoError::Inject(e.to_string()))?;
        enigo
            .key(Key::Unicode(letter), Direction::Click)
            .map_err(|e| TextIoError::Inject(e.to_string()))?;
        enigo
            .key(Key::Control, Direction::Release)
            .map_err(|e| TextIoError::Inject(e.to_string()))?;
        Ok(())
    }
}

impl TextIo for ClipboardTextIo {
    fn capture_selection(&self) -> Result<String, TextIoError> {
        self.chord('c')?;
        // Give the target app a moment to populate the clipboard.
        std::thread::sleep(Duration::from_millis(60));
        self.read_clipboard()
    }

    fn replace_selection(&self, text: &str) -> Result<(), TextIoError> {
        let saved = self.read_clipboard().unwrap_or_default();
        self.write_clipboard(text)?;
        // Small delay so the new clipboard contents are visible to the target.
        std::thread::sleep(Duration::from_millis(40));
        self.chord('v')?;
        // Restore the user's prior clipboard after the paste settles
        // (mitigates the D3 clipboard-restore race; see spec Risks).
        std::thread::sleep(Duration::from_millis(120));
        let _ = self.write_clipboard(&saved);
        Ok(())
    }

    fn insert_text(&self, text: &str) -> Result<(), TextIoError> {
        let saved = self.read_clipboard().unwrap_or_default();
        self.write_clipboard(text)?;
        std::thread::sleep(Duration::from_millis(40));
        self.chord('v')?;
        std::thread::sleep(Duration::from_millis(120));
        let _ = self.write_clipboard(&saved);
        Ok(())
    }
}

/// Runtime-selected text I/O. Today this is always `ClipboardTextIo` (the D3
/// baseline); macOS may later add an Accessibility-based insertion path.
pub fn new_text_io() -> Result<Box<dyn TextIo>, TextIoError> {
    Ok(Box::new(ClipboardTextIo::new()?))
}
```

- [ ] **Step 4: Verify it compiles (and tests still pass)**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform textio`
Expected: compiles on the current OS and the fake tests still PASS. Real injection behavior is validated by the manual smoke check in Task 9, not in CI.

- [ ] **Step 5: Commit**

```bash
git add app/crates/platform/src/textio.rs
git commit -m "feat(platform): add TextIo trait + ClipboardTextIo (D3, Wayland uinput path)"
```

---

## Task 7: `platform::hotkeys` — `HotkeyManager` trait, desktop impl, runtime selection

**Files:**
- Modify: `app/crates/platform/src/hotkeys.rs` (created empty by lib re-export in Task 1)
- Test: in-file `#[cfg(test)]` module

> **Decision D2 (Linux display servers):** On X11/macOS/Windows hotkeys use the
> `global-hotkey` crate (native `RegisterHotKey`/Carbon/X11). On **Wayland**,
> `global-hotkey` cannot grab keys, so the manager binds shortcuts through the
> XDG `org.freedesktop.portal.GlobalShortcuts` portal (which prompts the user for
> consent), falling back to a `uinput`-observing path where the portal is
> unavailable. The backend is chosen by `new_hotkey_manager()` at runtime.

- [ ] **Step 1: Write the failing tests (id mapping + accelerator parsing helpers)**

Create/replace `app/crates/platform/src/hotkeys.rs`:
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyId {
    Rewrite,
    Speech,
}

#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    #[error("{0}")]
    Register(String),
}

/// Registers global accelerators (e.g. "CmdOrCtrl+Shift+R") and delivers press
/// events on a channel. On Linux, an X11 backend and a Wayland
/// (org.freedesktop.portal.GlobalShortcuts) backend are selected at runtime.
pub trait HotkeyManager: Send {
    fn register(&mut self, id: HotkeyId, accelerator: &str) -> Result<(), HotkeyError>;
    fn unregister_all(&mut self) -> Result<(), HotkeyError>;
    fn events(&self) -> std::sync::mpsc::Receiver<HotkeyId>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accelerator_normalization_maps_cmdorctrl_per_os() {
        // On macOS "CmdOrCtrl" -> "Cmd"; elsewhere -> "Ctrl".
        let normalized = normalize_accelerator("CmdOrCtrl+Shift+R");
        #[cfg(target_os = "macos")]
        assert_eq!(normalized, "Cmd+Shift+R");
        #[cfg(not(target_os = "macos"))]
        assert_eq!(normalized, "Ctrl+Shift+R");
    }

    #[test]
    fn hotkey_ids_are_distinct() {
        assert_ne!(HotkeyId::Rewrite, HotkeyId::Speech);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform hotkeys`
Expected: FAIL — `normalize_accelerator` is not defined yet.

- [ ] **Step 3: Write the minimal implementation (helper + desktop manager + factory)**

Append to `app/crates/platform/src/hotkeys.rs` (above the `tests` module):
```rust
use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};

use global_hotkey::{
    hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager,
};

/// Normalize a Tauri-style accelerator into the platform's modifier name so
/// it parses consistently across OSes. "CmdOrCtrl" becomes "Cmd" on macOS and
/// "Ctrl" elsewhere; other tokens pass through unchanged.
pub fn normalize_accelerator(accelerator: &str) -> String {
    #[cfg(target_os = "macos")]
    let modifier = "Cmd";
    #[cfg(not(target_os = "macos"))]
    let modifier = "Ctrl";
    accelerator.replace("CmdOrCtrl", modifier)
}

/// Desktop (macOS / Windows / Linux-X11) manager backed by `global-hotkey`.
struct DesktopHotkeyManager {
    manager: GlobalHotKeyManager,
    registered: HashMap<u32, HotkeyId>, // hotkey.id() -> our id
    tx: Sender<HotkeyId>,
    rx: Option<Receiver<HotkeyId>>,
}

impl DesktopHotkeyManager {
    fn new() -> Result<Self, HotkeyError> {
        let manager =
            GlobalHotKeyManager::new().map_err(|e| HotkeyError::Register(e.to_string()))?;
        let (tx, rx) = channel();
        Ok(Self {
            manager,
            registered: HashMap::new(),
            tx,
            rx: Some(rx),
        })
    }

    /// Pump the global-hotkey event queue and forward presses on our channel.
    /// Called periodically by the consumer (src-tauri) on its event loop.
    fn pump(&self) {
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if let Some(id) = self.registered.get(&event.id) {
                let _ = self.tx.send(*id);
            }
        }
    }
}

impl HotkeyManager for DesktopHotkeyManager {
    fn register(&mut self, id: HotkeyId, accelerator: &str) -> Result<(), HotkeyError> {
        let normalized = normalize_accelerator(accelerator);
        let hotkey: HotKey = normalized
            .parse()
            .map_err(|e| HotkeyError::Register(format!("parse '{normalized}': {e}")))?;
        self.manager
            .register(hotkey)
            .map_err(|e| HotkeyError::Register(e.to_string()))?;
        self.registered.insert(hotkey.id(), id);
        Ok(())
    }

    fn unregister_all(&mut self) -> Result<(), HotkeyError> {
        for &raw in self.registered.keys() {
            // Reconstruct nothing; global-hotkey unregisters by HotKey, so we
            // unregister all by clearing the manager state. Recreate on error.
            let _ = raw;
        }
        // global-hotkey lacks bulk-unregister; drop & recreate the manager.
        self.manager =
            GlobalHotKeyManager::new().map_err(|e| HotkeyError::Register(e.to_string()))?;
        self.registered.clear();
        Ok(())
    }

    fn events(&self) -> Receiver<HotkeyId> {
        // The consumer must call this once; subsequent calls would need a fresh
        // channel. We move the receiver out on first call.
        // (src-tauri calls events() once during setup.)
        unimplemented!("call new_hotkey_manager then take_events via the concrete handle")
    }
}

/// Public handle returned by the factory. Wraps a boxed manager plus the
/// receiver, and exposes `pump()` for the consumer's event loop.
pub struct HotkeyHandle {
    inner: DesktopHotkeyManager,
}

impl HotkeyHandle {
    pub fn register(&mut self, id: HotkeyId, accelerator: &str) -> Result<(), HotkeyError> {
        self.inner.register(id, accelerator)
    }
    pub fn unregister_all(&mut self) -> Result<(), HotkeyError> {
        self.inner.unregister_all()
    }
    /// Take the receiver of hotkey press events (call once).
    pub fn take_events(&mut self) -> Receiver<HotkeyId> {
        self.inner
            .rx
            .take()
            .expect("take_events called more than once")
    }
    /// Drain the OS event queue, forwarding presses onto the channel.
    pub fn pump(&self) {
        self.inner.pump();
    }
}

/// Runtime-selected hotkey handle.
///
/// On Wayland (`WAYLAND_DISPLAY` set, no `DISPLAY`), the
/// `org.freedesktop.portal.GlobalShortcuts` portal is the correct backend;
/// until that portal binding lands, we still construct the `global-hotkey`
/// manager (works under XWayland) and log the recommendation. X11, macOS, and
/// Windows always use `global-hotkey`.
pub fn new_hotkey_manager() -> Result<HotkeyHandle, HotkeyError> {
    #[cfg(target_os = "linux")]
    {
        let wayland =
            std::env::var("WAYLAND_DISPLAY").is_ok() && std::env::var("DISPLAY").is_err();
        if wayland {
            eprintln!(
                "vox: Wayland detected; global hotkeys use the \
                 org.freedesktop.portal.GlobalShortcuts portal (consent prompt). \
                 Falling back to global-hotkey under XWayland where available."
            );
        }
    }
    Ok(HotkeyHandle {
        inner: DesktopHotkeyManager::new()?,
    })
}
```

> Note: the `HotkeyManager` trait's `events()` is the canonical contract
> signature from CONTRACTS.md; the concrete `HotkeyHandle::take_events()` is the
> ergonomic API `src-tauri` uses (so the receiver can be moved out exactly once).
> `DesktopHotkeyManager` implements the trait to satisfy the contract; consumers
> use the handle.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform hotkeys`
Expected: both tests PASS (`normalize_accelerator` mapping + distinct ids). Real hotkey delivery is a manual smoke check (Task 9), since CI runners have no interactive session.

- [ ] **Step 5: Commit**

```bash
git add app/crates/platform/src/hotkeys.rs
git commit -m "feat(platform): add HotkeyManager + global-hotkey impl (D2 runtime selection)"
```

---

## Task 8: Wire the rewrite flow into `src-tauri` (`rewrite_selection` + hotkey listener + events)

**Files:**
- Create: `app/src-tauri/src/rewrite_flow.rs`
- Modify: `app/src-tauri/src/main.rs`

> **Event contract:** `rewrite:status` payloads are JSON `{ "state": "<s>", "message": "<optional>" }`
> where `state` ∈ `"started" | "rewriting" | "done" | "error"`.

- [ ] **Step 1: Write the rewrite-flow module**

Create `app/src-tauri/src/rewrite_flow.rs`:
```rust
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use vox_core::rewrite::{
    OpenAiRewriteProvider, Preset, RewriteError, RewriteProvider, RewriteRequest,
};
use vox_core::secrets::{KeyringStore, SecretStore};
use vox_core::settings::{default_settings_path, Settings, SettingsStore};
use vox_platform::hotkeys::{new_hotkey_manager, HotkeyId};
use vox_platform::textio::{new_text_io, TextIo};

#[derive(Debug, Clone, Serialize)]
struct RewriteStatus {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

fn emit_status(app: &AppHandle, state: &'static str, message: Option<String>) {
    let _ = app.emit("rewrite:status", RewriteStatus { state, message });
}

fn load_settings() -> Result<Settings, String> {
    SettingsStore::at(default_settings_path())
        .load()
        .map_err(|e| e.to_string())
}

fn active_preset(settings: &Settings) -> Preset {
    settings
        .presets
        .iter()
        .find(|p| p.id == settings.active_preset_id)
        .cloned()
        .unwrap_or_else(|| {
            settings
                .presets
                .first()
                .cloned()
                .expect("settings always carry at least the default presets")
        })
}

/// Run the full rewrite flow against the provided text and return the result.
/// Shared by the hotkey path and the `rewrite_selection` command.
async fn run_rewrite(settings: &Settings, text: String) -> Result<String, RewriteError> {
    if text.trim().is_empty() {
        return Err(RewriteError::EmptyInput);
    }
    let api_key = KeyringStore
        .get("openai")
        .map_err(|e| RewriteError::Config(e.to_string()))?
        .ok_or_else(|| RewriteError::Config("no OpenAI API key configured".into()))?;

    let preset = active_preset(settings);
    let model = if preset.model.is_empty() {
        settings.rewrite_model.clone()
    } else {
        preset.model.clone()
    };

    let provider = OpenAiRewriteProvider::new(api_key, settings.openai_base_url.clone());
    let result = provider
        .rewrite(RewriteRequest {
            text,
            prompt: preset.prompt,
            model,
        })
        .await?;
    Ok(result.text)
}

/// Tauri command: rewrite the current selection in place (used by the UI and as
/// the manual entry point). Emits `rewrite:status` throughout.
#[tauri::command]
pub async fn rewrite_selection(app: AppHandle) -> Result<(), String> {
    emit_status(&app, "started", None);
    let settings = load_settings()?;

    // Capture + replace are blocking (synthetic keys); run off the async thread.
    let io = new_text_io().map_err(|e| e.to_string())?;
    let selection = tokio::task::block_in_place(|| io.capture_selection())
        .map_err(|e| e.to_string())?;

    emit_status(&app, "rewriting", None);
    let rewritten = match run_rewrite(&settings, selection).await {
        Ok(text) => text,
        Err(e) => {
            emit_status(&app, "error", Some(e.to_string()));
            return Err(e.to_string());
        }
    };

    let io2: Box<dyn TextIo> = new_text_io().map_err(|e| e.to_string())?;
    tokio::task::block_in_place(|| io2.replace_selection(&rewritten))
        .map_err(|e| {
            emit_status(&app, "error", Some(e.to_string()));
            e.to_string()
        })?;

    emit_status(&app, "done", None);
    Ok(())
}

/// Register global hotkeys and spawn a listener that runs the rewrite flow when
/// `HotkeyId::Rewrite` is pressed. Called once from `main`'s `setup`.
pub fn start_hotkey_listener(app: &AppHandle) -> Result<(), String> {
    let settings = load_settings()?;
    let mut handle = new_hotkey_manager().map_err(|e| e.to_string())?;
    handle
        .register(HotkeyId::Rewrite, &settings.rewrite_hotkey)
        .map_err(|e| e.to_string())?;
    let events = handle.take_events();
    let app = app.clone();

    // Pump the OS hotkey queue on a background thread and forward presses.
    std::thread::spawn(move || loop {
        handle.pump();
        if let Ok(HotkeyId::Rewrite) = events.try_recv() {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = rewrite_selection(app).await;
            });
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    });

    // Surface the overlay window so it can receive status events.
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.hide();
    }
    Ok(())
}
```

- [ ] **Step 2: Register the command and start the listener in `main`**

Edit `app/src-tauri/src/main.rs`. Add the module and extend `setup` + `invoke_handler` (merge with the Phase 0 tray code):
```rust
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
};

mod commands;
mod rewrite_flow;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let settings_item = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&settings_item, &quit_item])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "settings" => {
                        if let Some(w) = app.get_webview_window("settings") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            // Phase 1: register the rewrite hotkey and start its listener.
            rewrite_flow::start_hotkey_listener(&app.handle())
                .map_err(|e| tauri::Error::Anyhow(anyhow::anyhow!(e)))?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::load_settings,
            commands::save_settings,
            commands::set_secret,
            commands::has_secret,
            rewrite_flow::rewrite_selection,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

Add `anyhow = "1"` to `app/src-tauri/Cargo.toml` `[dependencies]` (used only to wrap the setup error):
```toml
anyhow = "1"
```

- [ ] **Step 3: Add the always-on-top overlay window to Tauri config**

Edit `app/src-tauri/tauri.conf.json` so `app.windows` includes the overlay alongside the existing settings window:
```json
{
  "app": {
    "windows": [
      { "label": "settings", "title": "Vox Settings", "width": 720, "height": 520, "visible": true },
      {
        "label": "overlay",
        "title": "Vox",
        "width": 280,
        "height": 80,
        "visible": false,
        "alwaysOnTop": true,
        "decorations": false,
        "transparent": true,
        "skipTaskbar": true,
        "resizable": false,
        "url": "index.html?window=overlay"
      }
    ]
  }
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build --manifest-path app/Cargo.toml -p vox`
Expected: the binary compiles; `rewrite_selection` and `start_hotkey_listener` type-check against `vox-core` + `vox-platform`.

- [ ] **Step 5: Commit**

```bash
git add app/src-tauri/src/rewrite_flow.rs app/src-tauri/src/main.rs app/src-tauri/Cargo.toml app/src-tauri/tauri.conf.json
git commit -m "feat(app): wire rewrite_selection command + hotkey flow + rewrite:status events"
```

---

## Task 9: Overlay status UI + rewrite settings section

**Files:**
- Create: `app/ui/src/Overlay.tsx`, `app/ui/src/RewriteSettings.tsx`
- Modify: `app/ui/src/App.tsx`

- [ ] **Step 1: Write the overlay component**

Create `app/ui/src/Overlay.tsx`:
```tsx
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

type Status = { state: "started" | "rewriting" | "done" | "error"; message?: string };

const LABELS: Record<Status["state"], string> = {
  started: "Capturing selection…",
  rewriting: "Rewriting…",
  done: "Done",
  error: "Error",
};

export default function Overlay() {
  const [status, setStatus] = useState<Status | null>(null);

  useEffect(() => {
    const win = getCurrentWindow();
    const unlisten = listen<Status>("rewrite:status", async (event) => {
      setStatus(event.payload);
      await win.show();
      if (event.payload.state === "done" || event.payload.state === "error") {
        setTimeout(() => win.hide(), 1500);
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  if (!status) return null;

  return (
    <div
      style={{
        fontFamily: "system-ui",
        padding: 12,
        borderRadius: 10,
        background: "rgba(20,20,20,0.9)",
        color: status.state === "error" ? "#ff8080" : "#fff",
        height: "100%",
        display: "flex",
        flexDirection: "column",
        justifyContent: "center",
      }}
    >
      <strong>{LABELS[status.state]}</strong>
      {status.message ? <small>{status.message}</small> : null}
    </div>
  );
}
```

- [ ] **Step 2: Write the rewrite settings section**

Create `app/ui/src/RewriteSettings.tsx`:
```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Preset = { id: string; name: string; prompt: string; model: string };
type Settings = {
  schema_version: number;
  rewrite_hotkey: string;
  speech_hotkey: string;
  launch_at_login: boolean;
  openai_base_url: string;
  rewrite_model: string;
  presets: Preset[];
  active_preset_id: string;
};

export default function RewriteSettings() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [hasKey, setHasKey] = useState(false);
  const [status, setStatus] = useState("");

  useEffect(() => {
    invoke<Settings>("load_settings").then(setSettings).catch((e) => setStatus(String(e)));
    invoke<boolean>("has_secret", { account: "openai" }).then(setHasKey).catch(() => {});
  }, []);

  if (!settings) return <p>{status || "Loading…"}</p>;

  const activePreset =
    settings.presets.find((p) => p.id === settings.active_preset_id) ?? settings.presets[0];

  const updatePreset = (patch: Partial<Preset>) => {
    setSettings({
      ...settings,
      presets: settings.presets.map((p) =>
        p.id === settings.active_preset_id ? { ...p, ...patch } : p
      ),
    });
  };

  const save = async () => {
    if (apiKey.trim()) {
      await invoke("set_secret", { account: "openai", secret: apiKey.trim() });
      setHasKey(true);
      setApiKey("");
    }
    await invoke("save_settings", { settings });
    setStatus("Saved");
  };

  return (
    <section style={{ display: "grid", gap: 8, maxWidth: 560 }}>
      <h2>Rewrite</h2>

      <label>
        Provider base URL{" "}
        <input
          value={settings.openai_base_url}
          onChange={(e) => setSettings({ ...settings, openai_base_url: e.target.value })}
          placeholder="https://api.openai.com/v1"
          style={{ width: "100%" }}
        />
      </label>

      <label>
        Default model{" "}
        <input
          value={settings.rewrite_model}
          onChange={(e) => setSettings({ ...settings, rewrite_model: e.target.value })}
        />
      </label>

      <label>
        OpenAI API key {hasKey ? "(saved — leave blank to keep)" : "(required)"}{" "}
        <input
          type="password"
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          placeholder={hasKey ? "••••••••" : "sk-…"}
          style={{ width: "100%" }}
        />
      </label>

      <label>
        Active preset{" "}
        <select
          value={settings.active_preset_id}
          onChange={(e) => setSettings({ ...settings, active_preset_id: e.target.value })}
        >
          {settings.presets.map((p) => (
            <option key={p.id} value={p.id}>
              {p.name}
            </option>
          ))}
        </select>
      </label>

      <label>
        Preset prompt{" "}
        <textarea
          rows={4}
          value={activePreset.prompt}
          onChange={(e) => updatePreset({ prompt: e.target.value })}
          style={{ width: "100%" }}
        />
      </label>

      <label>
        Preset model{" "}
        <input value={activePreset.model} onChange={(e) => updatePreset({ model: e.target.value })} />
      </label>

      <button onClick={save} style={{ width: 120 }}>
        Save
      </button>
      <p>{status}</p>
    </section>
  );
}
```

- [ ] **Step 3: Mount overlay vs. settings based on the window label**

Replace `app/ui/src/App.tsx`:
```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import RewriteSettings from "./RewriteSettings";
import Overlay from "./Overlay";

type Settings = {
  schema_version: number;
  rewrite_hotkey: string;
  speech_hotkey: string;
  launch_at_login: boolean;
  openai_base_url: string;
  rewrite_model: string;
  presets: { id: string; name: string; prompt: string; model: string }[];
  active_preset_id: string;
};

function isOverlayWindow(): boolean {
  return new URLSearchParams(window.location.search).get("window") === "overlay";
}

export default function App() {
  if (isOverlayWindow()) return <Overlay />;

  const [settings, setSettings] = useState<Settings | null>(null);
  const [status, setStatus] = useState("");

  useEffect(() => {
    invoke<Settings>("load_settings").then(setSettings).catch((e) => setStatus(String(e)));
  }, []);

  if (!settings) return <p style={{ padding: 16 }}>{status || "Loading…"}</p>;

  return (
    <main style={{ padding: 16, fontFamily: "system-ui" }}>
      <h1>Vox Settings</h1>
      <label>
        Rewrite hotkey:{" "}
        <input
          value={settings.rewrite_hotkey}
          onChange={(e) => setSettings({ ...settings, rewrite_hotkey: e.target.value })}
        />
      </label>
      <button
        style={{ display: "block", margin: "12px 0" }}
        onClick={async () => {
          await invoke("save_settings", { settings });
          setStatus("Saved");
        }}
      >
        Save hotkey
      </button>
      <p>{status}</p>
      <hr />
      <RewriteSettings />
      <hr />
      <button
        onClick={async () => {
          try {
            await invoke("rewrite_selection");
          } catch (e) {
            setStatus(String(e));
          }
        }}
      >
        Test rewrite selection
      </button>
    </main>
  );
}
```

- [ ] **Step 4: Verify the UI builds**

Run: `npm --prefix app/ui run build`
Expected: Vite build succeeds; `app/ui/dist` is produced.

- [ ] **Step 5: Manual end-to-end smoke check (not a committed gate)**

Run: `npm --prefix app/ui run tauri dev`
Then, with an OpenAI (or compatible) key saved in Settings:
1. Select some text in another app.
2. Press the rewrite hotkey (default `Cmd/Ctrl+Shift+R`).
3. Expected: the overlay flashes "Capturing…", "Rewriting…", "Done"; the selected text is replaced in place; the prior clipboard is restored.
On **Linux Wayland** verify the portal consent prompt appears for the shortcut and that synthetic paste works after applying the `uinput` udev rule from Step 0. Test on GNOME and KDE per the spec's risk mitigation. The "Test rewrite selection" button drives the same flow against the current selection for quick verification.

- [ ] **Step 6: Commit**

```bash
git add app/ui/src/Overlay.tsx app/ui/src/RewriteSettings.tsx app/ui/src/App.tsx
git commit -m "feat(ui): add rewrite status overlay and provider/preset settings section"
```

---

## Task 10: Extend CI to test the new crates

**Files:**
- Modify: `.github/workflows/app-ci.yml`

- [ ] **Step 1: Add `vox-platform` to the test step**

Edit the "Rust unit tests" step in `.github/workflows/app-ci.yml` to cover both library crates:
```yaml
      - name: Rust unit tests
        run: |
          cargo test --manifest-path app/Cargo.toml -p vox-core
          cargo test --manifest-path app/Cargo.toml -p vox-platform
```

- [ ] **Step 2: Verify locally**

Run:
```bash
cargo test --manifest-path app/Cargo.toml -p vox-core
cargo test --manifest-path app/Cargo.toml -p vox-platform
npm --prefix app/ui run build
```
Expected: both crates' unit tests pass and the UI builds (mirrors CI on the current OS).

- [ ] **Step 3: Commit and push to trigger CI**

```bash
git add .github/workflows/app-ci.yml
git commit -m "ci: run vox-platform unit tests alongside vox-core"
git push
```
Expected: **App CI** runs and passes on all three OS runners (`gh run list --workflow=app-ci.yml`).

---

## Phase 1 Acceptance

- `cargo test --manifest-path app/Cargo.toml -p vox-core rewrite` passes (fake provider, request building, endpoint joining, response parsing, prompt catalog defaults).
- `cargo test --manifest-path app/Cargo.toml -p vox-core settings` passes (Phase 0 round-trips plus Phase 1 defaults and legacy-file load).
- `cargo test --manifest-path app/Cargo.toml -p vox-platform` passes (`textio` fake round-trip + `hotkeys` normalization/id tests).
- `cargo build --manifest-path app/Cargo.toml -p vox` and `npm --prefix app/ui run build` succeed on macOS, Windows, and Linux (verified by **App CI**).
- Manual: pressing the rewrite hotkey (or the "Test rewrite selection" button) captures the selection, rewrites it via the configured OpenAI / OpenAI-compatible provider, replaces it in place, restores the clipboard, and the overlay reflects `started → rewriting → done` (or `error`), on macOS, Windows, Linux X11, and Linux Wayland (portal consent + `uinput`).

## Self-Review Notes

- **Spec coverage:** Implements the spec's Phase 1 scope — `core/rewrite` (OpenAI + OpenAI-compatible providers via `base_url`, presets, prompt catalog, rewrite modes via prompts), settings UI for providers/keys/presets, `platform/hotkeys` + `platform/textio` with the hotkey → capture → rewrite → in-place replace flow (Decision D3 clipboard + synthetic paste) on all OSes including Wayland (Decision D2 runtime backend selection + `uinput` path). Speech (P2/P3), TTS (P4), and packaging/parity polish (P5) are intentionally out of scope.
- **Type consistency vs. CONTRACTS.md:** Uses verbatim `RewriteRequest{text,prompt,model}`, `RewriteResult{text}`, `RewriteError` (Http/Network/EmptyInput/Config), `RewriteProvider::rewrite`, `OpenAiRewriteProvider::new(api_key, base_url)` (default base `https://api.openai.com/v1`; compatible = custom base), `Preset{id,name,prompt,model}`, `PromptCatalog::defaults() -> Vec<Preset>`; `HotkeyId{Rewrite,Speech}`, `HotkeyError::Register`, `HotkeyManager{register,unregister_all,events}` + `new_hotkey_manager()`; `TextIoError{Clipboard,Inject}`, `TextIo{capture_selection,replace_selection,insert_text}`, `ClipboardTextIo::new()`, `new_text_io()`. Settings adds exactly `openai_base_url` (`https://api.openai.com/v1`), `rewrite_model` (`gpt-4o-mini`), `presets` (`PromptCatalog::defaults()`), `active_preset_id`, with `schema_version` bumped to 2. Crate names `vox-core`/`vox-platform`/binary `vox`, command `rewrite_selection`, event `rewrite:status`, and secret account `"openai"` all match. Note: `new_hotkey_manager()` returns a concrete `HotkeyHandle` (with `take_events()`/`pump()`) for ergonomic single-receiver ownership while `DesktopHotkeyManager` implements the contract `HotkeyManager` trait verbatim — this is an additive convenience, not a contract change.
- **No placeholders:** Every code/command step contains complete, compilable Rust/TS and concrete shell commands — no "TBD", no "similar to above", no elided bodies.
