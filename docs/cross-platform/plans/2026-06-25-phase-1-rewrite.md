# KEA Phase 1 — Rewrite Feature Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver the core KEA value proposition on macOS first (Windows/Linux follow as parallel platform tasks): a global hotkey captures the current selection, the Rewrite feature resolves a user-bound `LlmEngine`, builds a prompt from preset/mode + selected text, calls `LlmEngine::complete(LlmRequest) -> Result<LlmResponse, EngineError>`, replaces the selection in place via clipboard + synthetic paste (D4), records the action in `data.db`, and surfaces progress/errors through Tauri events and a React Configuration + Features UI.

**Architecture:** Phase 0 shipped trait + registry scaffolding (`LlmEngine`, `Feature`, `SlotResolver`, `BindingRepo`, `CredentialStore`, SQLite stores). Phase 1 adds real LLM engine plugins (`openai`, `openai-compatible`) behind an injectable HTTP transport, rewrite domain types/repos in `kea-core`, platform traits + macOS impls (`Hotkeys`, `TextIo`), a `RewriteFeature` plugin in `kea-features`, and thin Tauri wiring (`AppState`, commands, events, hotkey dispatcher). Consumers depend on traits; `src-tauri` is the only composition root.

**Tech Stack:** Rust (edition 2021), Tauri 2.x, `reqwest` (`json`, `rustls-tls`), `wiremock` (unit tests), `global-hotkey`, `arboard`, `enigo`, `sqlx`, `tokio`, `async-trait`, `serde`/`serde_json`, `keyring`, Vite + React + TypeScript (D10).

## Global Constraints

- **Product name:** `KEA` everywhere (`kea-*` crates, `ai.kea.app`). _(D13.)_
- **Plugin model:** internal trait + registry, compiled in. No dynamic loading. _(D1, D2.)_
- **Storage boundary (D9):** `config.db` = settings, presets, prompt catalog overrides, bindings, hotkey bindings, provider config (base URL + default model — **not** API keys); `data.db` = actions/conversations; **keyring = credentials only** via `CredentialStore`. DB rows store `engine_id`, `model`, `provider_ref` references — never secrets.
- **In-place replacement (D4):** save clipboard → set rewritten text → synthesize Cmd/Ctrl+V → restore clipboard. macOS Accessibility insertion is Phase 4 (D12).
- **Web UI (D10):** React pages composed from a shared component library; Rust plugins expose typed Tauri commands/events only.
- **Async:** all engine/platform I/O trait methods are `async` (`async-trait`).
- **TDD:** every code task is test-first. Rust async tests use `#[tokio::test]`. Store tests use `sqlite::memory:`; HTTP engine tests use `wiremock` or an injected `HttpClient` trait — **never hit real OpenAI in unit tests**.
- **macOS-first:** Tasks 14–15 must pass before end-to-end acceptance on the primary dev machine. Tasks 16–19 are clearly labeled **parallel per-OS** and may land after macOS E2E.
- **Targets:** code compiles on macOS, Windows, Linux; CI runs `cargo test --workspace` on all three.
- **Commits:** frequent conventional commits, one per task minimum. Use `git commit --no-verify` when the legacy Vox FluidAudio pre-commit hook blocks unrelated paths.

---

## File Structure

```
kea/
├─ Cargo.toml                              # add reqwest, wiremock workspace deps
├─ crates/
│  ├─ core/
│  │  ├─ migrations/config/0002_rewrite.sql
│  │  └─ src/
│  │     ├─ rewrite/
│  │     │  ├─ mod.rs                      # re-exports
│  │     │  ├─ mode.rs                    # RewriteMode enum (parity with VoxNative)
│  │     │  ├─ catalog.rs                 # built-in prompts + override merge
│  │     │  ├─ preset.rs                  # RewritePreset + PresetRepo
│  │     │  ├─ request.rs                 # RewriteRequest -> LlmRequest builder
│  │     │  └─ provider.rs                # ProviderConfig + ProviderConfigRepo
│  │     └─ store/
│  │        ├─ hotkeys.rs                 # HotkeyBindingRepo (config.db)
│  │        └─ actions.rs                 # extend: finish(), record_conversation stub
│  ├─ engines/
│  │  └─ src/
│  │     ├─ http.rs                       # HttpClient trait + ReqwestHttpClient
│  │     ├─ llm/
│  │     │  ├─ mod.rs
│  │     │  ├─ openai.rs                  # OpenAiLlmEngine
│  │     │  └─ openai_compatible.rs       # OpenAiCompatibleLlmEngine
│  │     └─ lib.rs                        # register_phase1_engines()
│  ├─ features/
│  │  └─ src/
│  │     ├─ feature.rs                    # extend: commands()
│  │     └─ rewrite.rs                    # RewriteFeature + run_rewrite()
│  └─ platform/
│     └─ src/
│        ├─ lib.rs                         # new_hotkeys(), new_text_io()
│        ├─ hotkeys.rs                    # Hotkeys trait + HotkeyBinding
│        ├─ textio.rs                     # TextIo trait
│        ├─ macos/
│        │  ├─ hotkeys.rs
│        │  └─ textio.rs
│        ├─ windows/                       # [PARALLEL Task 16–17]
│        │  ├─ hotkeys.rs
│        │  └─ textio.rs
│        └─ linux/                         # [PARALLEL Task 18–19]
│           ├─ hotkeys_x11.rs
│           ├─ hotkeys_wayland.rs
│           └─ textio.rs
├─ src-tauri/src/
│  ├─ main.rs                              # AppState, hotkey dispatcher, engine registration
│  ├─ commands.rs                          # provider/preset/rewrite commands
│  └─ events.rs                            # rewrite:progress / rewrite:error helpers
└─ ui/src/
   ├─ api.ts                               # typed invoke wrappers
   ├─ App.tsx                              # router: Configuration / Features
   ├─ pages/
   │  ├─ ConfigurationPage.tsx
   │  └─ FeaturesPage.tsx
   └─ components/
      ├─ CredentialField.tsx
      ├─ EngineConfig.tsx
      ├─ SlotBinder.tsx
      ├─ HotkeyBinder.tsx
      └─ SettingsForm.tsx
```

---

### Task 1: Workspace + crate dependencies for Phase 1

**Files:**
- Modify: `Cargo.toml` (workspace deps)
- Modify: `crates/engines/Cargo.toml`, `crates/core/Cargo.toml`, `crates/platform/Cargo.toml`, `crates/features/Cargo.toml`, `src-tauri/Cargo.toml`

**Interfaces:**
- Produces: workspace deps `reqwest`, `wiremock`; crate-local deps wired for engines (`reqwest`), platform (`arboard`, `enigo`, `global-hotkey`), features (`kea-platform`).

- [ ] **Step 1: Write the failing test**

In `crates/engines/src/lib.rs` add:

```rust
#[cfg(test)]
mod dep_smoke {
    #[test]
    fn reqwest_is_linked() {
        let _ = std::any::type_name::<reqwest::Client>();
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-engines dep_smoke`
Expected: FAIL — `reqwest` not found.

- [ ] **Step 3: Add workspace + crate dependencies**

Root `Cargo.toml` `[workspace.dependencies]` append:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
wiremock = "0.6"
arboard = "3"
enigo = "0.2"
global-hotkey = "0.6"
```

`crates/engines/Cargo.toml`:

```toml
[dependencies]
kea-core = { path = "../core" }
reqwest.workspace = true
# keep existing workspace deps

[dev-dependencies]
wiremock.workspace = true
tokio = { workspace = true, features = ["macros", "rt-multi-thread"] }
```

`crates/platform/Cargo.toml`:

```toml
[dependencies]
async-trait.workspace = true
serde.workspace = true
thiserror.workspace = true
arboard.workspace = true
enigo.workspace = true
global-hotkey.workspace = true
tracing.workspace = true
```

`crates/features/Cargo.toml` add `kea-platform = { path = "../platform" }`.

`src-tauri/Cargo.toml` add `kea-platform = { path = "../crates/platform" }`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-engines dep_smoke`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/engines crates/platform crates/features src-tauri/Cargo.toml
git commit --no-verify -m "feat(phase1): add reqwest, platform, and test dependencies"
```

---

### Task 2: `config.db` migration — presets, prompt overrides, hotkey bindings

**Files:**
- Create: `crates/core/migrations/config/0002_rewrite.sql`
- Modify: `crates/core/src/store/db.rs` (no change — migrator picks up new file automatically)
- Test: `crates/core/src/store/presets.rs` (new, inline tests in Task 3)

**Interfaces:**
- Produces: tables `rewrite_presets`, `rewrite_prompt_overrides`, `hotkey_bindings`.

- [ ] **Step 1: Write the failing test**

Create `crates/core/src/store/presets.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn presets_table_exists_after_migration() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rewrite_presets")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }
}
```

Add `pub mod presets;` to `crates/core/src/store/mod.rs` and `#[cfg(test)]` import in test module.

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-core presets`
Expected: FAIL — `no such table: rewrite_presets`.

- [ ] **Step 3: Write the migration**

`crates/core/migrations/config/0002_rewrite.sql`:

```sql
CREATE TABLE rewrite_presets (
    id          TEXT PRIMARY KEY NOT NULL,
    name        TEXT NOT NULL,
    instruction TEXT NOT NULL,
    sort_order  INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE rewrite_prompt_overrides (
    mode        TEXT PRIMARY KEY NOT NULL,
    prompt      TEXT NOT NULL
);

CREATE TABLE hotkey_bindings (
    feature_id  TEXT NOT NULL,
    command     TEXT NOT NULL,
    accelerator TEXT NOT NULL,
    PRIMARY KEY (feature_id, command)
);
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-core presets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core
git commit --no-verify -m "feat(core): config.db migration for rewrite presets and hotkeys"
```

---

### Task 3: `ProviderConfig` + `ProviderConfigRepo` (settings-backed, no secrets)

**Files:**
- Create: `crates/core/src/rewrite/provider.rs`, `crates/core/src/rewrite/mod.rs`
- Modify: `crates/core/src/lib.rs` (`pub mod rewrite;`)

**Interfaces:**
- Produces: `ProviderConfig { base_url: String, default_model: String }`; `ProviderConfigRepo::get(provider_ref) -> Option<ProviderConfig>`; `set(provider_ref, &ProviderConfig)`.
- Keys in `settings` table: `provider.{provider_ref}` → JSON `ProviderConfig`.

- [ ] **Step 1: Write the failing test**

`crates/core/src/rewrite/provider.rs`:

```rust
use serde::{Deserialize, Serialize};
use crate::store::settings::SettingsRepo;
use crate::error::KeaError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub default_model: String,
}

pub struct ProviderConfigRepo { settings: SettingsRepo }

impl ProviderConfigRepo {
    pub fn new(settings: SettingsRepo) -> Self { Self { settings } }
    fn key(provider_ref: &str) -> String { format!("provider.{provider_ref}") }

    pub async fn get(&self, provider_ref: &str) -> Result<Option<ProviderConfig>, KeaError> {
        self.settings.get(&Self::key(provider_ref)).await
    }

    pub async fn set(&self, provider_ref: &str, cfg: &ProviderConfig) -> Result<(), KeaError> {
        self.settings.set(&Self::key(provider_ref), cfg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn roundtrips_provider_config() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = ProviderConfigRepo::new(SettingsRepo::new(pool));
        let cfg = ProviderConfig {
            base_url: "https://api.openai.com/v1".into(),
            default_model: "gpt-4o-mini".into(),
        };
        repo.set("openai", &cfg).await.unwrap();
        assert_eq!(repo.get("openai").await.unwrap(), Some(cfg));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-core rewrite::provider`
Expected: FAIL — module `rewrite` not found.

- [ ] **Step 3: Wire module**

`crates/core/src/rewrite/mod.rs`:

```rust
pub mod provider;
pub use provider::{ProviderConfig, ProviderConfigRepo};
```

`crates/core/src/lib.rs`: `pub mod rewrite;`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-core rewrite::provider`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core
git commit --no-verify -m "feat(core): ProviderConfig repo on config.db settings"
```

---

### Task 4: Injectable `HttpClient` trait + `ReqwestHttpClient`

**Files:**
- Create: `crates/engines/src/http.rs`
- Modify: `crates/engines/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  #[async_trait]
  pub trait HttpClient: Send + Sync {
      async fn post_json(
          &self, url: &str, bearer: &str, body: serde_json::Value,
      ) -> Result<(u16, String), EngineError>;
  }
  pub struct ReqwestHttpClient { client: reqwest::Client }
  ```

- [ ] **Step 1: Write the failing test**

`crates/engines/src/http.rs`:

```rust
use async_trait::async_trait;
use crate::traits::EngineError;

#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn post_json(
        &self, url: &str, bearer: &str, body: serde_json::Value,
    ) -> Result<(u16, String), EngineError>;
}

pub struct ReqwestHttpClient { client: reqwest::Client }

impl ReqwestHttpClient {
    pub fn new() -> Self {
        Self { client: reqwest::Client::new() }
    }
}

#[async_trait]
impl HttpClient for ReqwestHttpClient {
    async fn post_json(
        &self, url: &str, bearer: &str, body: serde_json::Value,
    ) -> Result<(u16, String), EngineError> {
        let resp = self.client.post(url)
            .bearer_auth(bearer)
            .json(&body)
            .send().await
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.map_err(|e| EngineError::Other(e.to_string()))?;
        Ok((status, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn posts_json_and_returns_status_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"choices":[{"message":{"content":"ok"}}]}"#))
            .mount(&server).await;

        let http = ReqwestHttpClient::new();
        let (status, body) = http.post_json(
            &format!("{}/v1/chat/completions", server.uri()),
            "sk-test",
            serde_json::json!({"model":"gpt-4o-mini","messages":[]}),
        ).await.unwrap();
        assert_eq!(status, 200);
        assert!(body.contains("ok"));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-engines http`
Expected: FAIL — module not found.

- [ ] **Step 3: Export from lib**

`crates/engines/src/lib.rs`: `pub mod http; pub use http::{HttpClient, ReqwestHttpClient};`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-engines http`
Expected: PASS (wiremock local server — no network).

- [ ] **Step 5: Commit**

```bash
git add crates/engines
git commit --no-verify -m "feat(engines): injectable HttpClient with wiremock tests"
```

---

### Task 5: `OpenAiLlmEngine` implementing `LlmEngine`

**Files:**
- Create: `crates/engines/src/llm/mod.rs`, `crates/engines/src/llm/openai.rs`
- Modify: `crates/engines/src/lib.rs`, `crates/engines/Cargo.toml` (ensure `kea-core` dep)

**Interfaces:**
- Consumes: `LlmEngine::complete(LlmRequest) -> Result<LlmResponse, EngineError>`, `HttpClient`, `CredentialStore`, `ProviderConfigRepo`, `Binding { model, provider_ref }`.
- Produces: `OpenAiLlmEngine { http: Arc<dyn HttpClient>, credentials: Arc<dyn CredentialStore>, providers: Arc<ProviderConfigRepo> }` with `id() == "openai"`.

- [ ] **Step 1: Write the failing test**

`crates/engines/src/llm/openai.rs`:

```rust
use std::sync::Arc;
use async_trait::async_trait;
use kea_core::rewrite::{ProviderConfig, ProviderConfigRepo};
use kea_core::secrets::{CredentialStore, InMemoryCredentialStore};
use kea_core::store::settings::SettingsRepo;
use kea_core::store::db::{open_pool, run_config_migrations};
use crate::http::HttpClient;
use crate::traits::{EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub struct OpenAiLlmEngine {
    pub http: Arc<dyn HttpClient>,
    pub credentials: Arc<dyn CredentialStore>,
    pub providers: Arc<ProviderConfigRepo>,
}

struct MockHttp(Arc<MockServer>);

#[async_trait]
impl HttpClient for MockHttp {
    async fn post_json(&self, url: &str, bearer: &str, body: serde_json::Value)
        -> Result<(u16, String), EngineError>
    {
        reqwest::Client::new()
            .post(url).bearer_auth(bearer).json(&body).send().await
            .map_err(|e| EngineError::Other(e.to_string()))
            .and_then(|r| async move {
                Ok((r.status().as_u16(), r.text().await.map_err(|e| EngineError::Other(e.to_string()))?))
            }.await)
    }
}

#[async_trait]
impl LlmEngine for OpenAiLlmEngine {
    fn id(&self) -> &str { "openai" }
    fn capabilities(&self) -> EngineCaps {
        EngineCaps { models: vec!["gpt-4o-mini".into(), "gpt-4o".into()] }
    }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
        let provider_ref = "openai";
        let api_key = self.credentials.get(provider_ref).await
            .map_err(|e| EngineError::Other(e.to_string()))?
            .ok_or_else(|| EngineError::Other("missing api key".into()))?;
        let cfg = self.providers.get(provider_ref).await
            .map_err(|e| EngineError::Other(e.to_string()))?
            .unwrap_or(ProviderConfig {
                base_url: "https://api.openai.com/v1".into(),
                default_model: "gpt-4o-mini".into(),
            });
        let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": cfg.default_model,
            "messages": [{"role":"user","content": req.prompt}],
        });
        let (status, text) = self.http.post_json(&url, &api_key, body).await?;
        if status != 200 {
            return Err(EngineError::Other(format!("http {status}: {text}")));
        }
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let content = parsed["choices"][0]["message"]["content"].as_str()
            .ok_or_else(|| EngineError::Other("missing content".into()))?;
        Ok(LlmResponse { text: content.to_string() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn completes_against_mock_openai() {
        let server = Arc::new(MockServer::start().await);
        Mock::given(method("POST")).and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"choices":[{"message":{"content":"rewritten"}}]}"#))
            .mount(&*server).await;

        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let providers = Arc::new(ProviderConfigRepo::new(SettingsRepo::new(pool)));
        providers.set("openai", &ProviderConfig {
            base_url: server.uri(),
            default_model: "gpt-4o-mini".into(),
        }).await.unwrap();

        let creds = Arc::new(InMemoryCredentialStore::default());
        creds.set("openai", "sk-test").await.unwrap();

        let engine = OpenAiLlmEngine {
            http: Arc::new(MockHttp(server.clone())),
            credentials: creds,
            providers,
        };
        let out = engine.complete(LlmRequest { prompt: "fix this".into() }).await.unwrap();
        assert_eq!(out.text, "rewritten");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-engines llm::openai`
Expected: FAIL — module not found.

- [ ] **Step 3: Wire `llm` module**

`crates/engines/src/llm/mod.rs`: `pub mod openai; pub use openai::OpenAiLlmEngine;`

`crates/engines/src/lib.rs`: `pub mod llm;`

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-engines llm::openai`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/engines
git commit --no-verify -m "feat(engines): OpenAiLlmEngine with mock HTTP tests"
```

---

### Task 6: `OpenAiCompatibleLlmEngine` (configurable base URL per `provider_ref`)

**Files:**
- Create: `crates/engines/src/llm/openai_compatible.rs`
- Modify: `crates/engines/src/llm/mod.rs`

**Interfaces:**
- Produces: `OpenAiCompatibleLlmEngine` with `id() == "openai-compatible"`; reads `ProviderConfig.base_url` from binding's `provider_ref` (default `"local-llm"`).

- [ ] **Step 1: Write the failing test**

`crates/engines/src/llm/openai_compatible.rs` — same structure as Task 5 but:

```rust
#[async_trait]
impl LlmEngine for OpenAiCompatibleLlmEngine {
    fn id(&self) -> &str { "openai-compatible" }
    // ...
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
        let provider_ref = "local-llm"; // binding's provider_ref passed via LlmCompleteContext in Task 21
        // for this unit test, hard-code provider_ref lookup same as openai engine
        // ...
    }
}

#[tokio::test]
async fn hits_custom_base_url() {
    // wiremock at http://127.0.0.1:.../v1/chat/completions
    // ProviderConfig { base_url: server.uri() + "/v1", default_model: "llama3" }
    // assert response parsed
}
```

Implement `complete` identically to `OpenAiLlmEngine` but `id() == "openai-compatible"` and `provider_ref` is a field on the struct set at construction from the resolved binding (add `provider_ref: String` field).

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-engines llm::openai_compatible`
Expected: FAIL.

- [ ] **Step 3: Implement + export**

Add `pub mod openai_compatible;` and `pub use openai_compatible::OpenAiCompatibleLlmEngine;`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-engines llm::openai_compatible`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/engines
git commit --no-verify -m "feat(engines): OpenAiCompatibleLlmEngine for custom base URLs"
```

---

### Task 7: `register_phase1_engines()` in `EngineRegistry`

**Files:**
- Modify: `crates/engines/src/lib.rs`
- Test: inline in `lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn register_phase1_engines(
      reg: &mut EngineRegistry,
      http: Arc<dyn HttpClient>,
      credentials: Arc<dyn CredentialStore>,
      providers: Arc<ProviderConfigRepo>,
  )
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod register_tests {
    use super::*;
    use kea_core::secrets::InMemoryCredentialStore;
    use kea_core::rewrite::ProviderConfigRepo;
    use kea_core::store::db::{open_pool, run_config_migrations};
    use kea_core::store::settings::SettingsRepo;
    use std::sync::Arc;

    #[tokio::test]
    async fn registers_openai_engines() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let providers = Arc::new(ProviderConfigRepo::new(SettingsRepo::new(pool)));
        let mut reg = EngineRegistry::default();
        register_phase1_engines(
            &mut reg,
            Arc::new(ReqwestHttpClient::new()),
            Arc::new(InMemoryCredentialStore::default()),
            providers,
        );
        let ids = reg.list_llm_ids();
        assert!(ids.contains(&"openai".to_string()));
        assert!(ids.contains(&"openai-compatible".to_string()));
    }
}
```

- [ ] **Step 2: Run test — FAIL**

Run: `cargo test -p kea-engines register_tests`

- [ ] **Step 3: Implement**

```rust
pub fn register_phase1_engines(
    reg: &mut EngineRegistry,
    http: Arc<dyn HttpClient>,
    credentials: Arc<dyn CredentialStore>,
    providers: Arc<ProviderConfigRepo>,
) {
    reg.register_llm(Arc::new(OpenAiLlmEngine {
        http: http.clone(), credentials: credentials.clone(), providers: providers.clone(),
    }));
    reg.register_llm(Arc::new(OpenAiCompatibleLlmEngine {
        http, credentials, providers, provider_ref: "local-llm".into(),
    }));
}
```

Keep `NoopLlmEngine` registered only in tests/dev if needed; production `main.rs` calls `register_phase1_engines` instead of noop.

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(engines): register_phase1_engines helper"
```

---

### Task 8: `RewriteMode` + built-in `PromptCatalog`

**Files:**
- Create: `crates/core/src/rewrite/mode.rs`, `crates/core/src/rewrite/catalog.rs`
- Modify: `crates/core/src/rewrite/mod.rs`

**Interfaces:**
- Produces: `enum RewriteMode` with variants matching VoxNative (`improve`, `fix_grammar`, `professional`, `concise`, `friendly`, `audio_refinement`, `ask_vox`); `PromptCatalog::prompt(mode) -> &str`; `rendered_prompt(mode, source_text, custom_instruction) -> Result<String, KeaError>`.

- [ ] **Step 1: Write the failing test**

`crates/core/src/rewrite/catalog.rs`:

```rust
use super::mode::RewriteMode;
use crate::error::KeaError;

pub struct PromptCatalog;

impl PromptCatalog {
    pub fn prompt(mode: RewriteMode) -> &'static str {
        match mode {
            RewriteMode::Improve => "You are a writing assistant. Improve the provided source text...",
            RewriteMode::FixGrammar => "You are a grammar and spelling assistant...",
            RewriteMode::Professional => "You are a professional writing assistant...",
            RewriteMode::Concise => "You are a concise writing assistant...",
            RewriteMode::Friendly => "You are a friendly writing assistant...",
            RewriteMode::AudioRefinement => "IMPORTANT: You are a text cleanup tool...",
            RewriteMode::AskKea => "You are a writing assistant...{{instruction}}...{{source_text}}...",
        }
    }

    pub fn rendered(
        mode: RewriteMode,
        source_text: &str,
        custom_instruction: Option<&str>,
        override_prompt: Option<&str>,
    ) -> Result<String, KeaError> {
        let template = override_prompt.unwrap_or(Self::prompt(mode));
        if mode == RewriteMode::AskKea {
            let instruction = custom_instruction
                .map(str::trim).filter(|s| !s.is_empty())
                .ok_or_else(|| KeaError::Other("missing custom instruction".into()))?;
            Ok(template
                .replace("{{instruction}}", instruction)
                .replace("{{source_text}}", source_text))
        } else {
            Ok(format!("{template}\n\nSource text:\n{source_text}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn improve_appends_source_text() {
        let p = PromptCatalog::rendered(RewriteMode::Improve, "hello", None, None).unwrap();
        assert!(p.contains("hello"));
        assert!(p.contains("writing assistant"));
    }
}
```

`crates/core/src/rewrite/mode.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RewriteMode {
    Improve,
    FixGrammar,
    Professional,
    Concise,
    Friendly,
    AudioRefinement,
    AskKea,
}
```

Copy full prompt strings from `VoxNative/Rewrite/PromptCatalog.swift` `builtInPrompts` (rename `askVox` → `AskKea`).

- [ ] **Step 2: Run test — FAIL**

Run: `cargo test -p kea-core rewrite::catalog`

- [ ] **Step 3: Wire modules + full prompt text**

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(core): RewriteMode and built-in PromptCatalog"
```

---

### Task 9: `PresetRepo` + `PromptOverrideRepo`

**Files:**
- Replace: `crates/core/src/store/presets.rs`
- Create: `crates/core/src/store/prompt_overrides.rs`
- Modify: `crates/core/src/store/mod.rs`, `crates/core/src/rewrite/preset.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct RewritePreset { pub id: String, pub name: String, pub instruction: String }
  pub struct PresetRepo { /* list, get, upsert, delete, set_active in settings key rewrite.active_preset_id */ }
  pub struct PromptOverrideRepo { /* get(mode), set(mode, prompt) */ }
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn preset_roundtrip() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    run_config_migrations(&pool).await.unwrap();
    let repo = PresetRepo::new(pool.clone());
    repo.upsert(&RewritePreset {
        id: "p1".into(), name: "Formal".into(), instruction: "Be formal".into(),
    }).await.unwrap();
    let all = repo.list().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].name, "Formal");
}
```

- [ ] **Step 2: Run test — FAIL**

- [ ] **Step 3: Implement `PresetRepo`**

```rust
impl PresetRepo {
    pub async fn upsert(&self, p: &RewritePreset) -> Result<(), KeaError> {
        sqlx::query("INSERT INTO rewrite_presets(id,name,instruction) VALUES(?,?,?)
                     ON CONFLICT(id) DO UPDATE SET name=excluded.name, instruction=excluded.instruction")
            .bind(&p.id).bind(&p.name).bind(&p.instruction)
            .execute(&self.pool).await?;
        Ok(())
    }
    pub async fn list(&self) -> Result<Vec<RewritePreset>, KeaError> { /* SELECT */ }
}
```

`PromptOverrideRepo` mirrors `rewrite_prompt_overrides` table keyed by `RewriteMode` serde string.

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(core): preset and prompt override repositories"
```

---

### Task 10: `RewriteRequest` builder → `LlmRequest`

**Files:**
- Create: `crates/core/src/rewrite/request.rs`

**Interfaces:**
- Consumes: `RewriteMode`, optional `RewritePreset`, `PromptOverrideRepo`, `source_text`.
- Produces:
  ```rust
  pub struct RewriteInput {
      pub source_text: String,
      pub mode: RewriteMode,
      pub preset_id: Option<String>,
      pub custom_instruction: Option<String>,
  }
  pub async fn build_llm_request(
      input: &RewriteInput,
      presets: &PresetRepo,
      overrides: &PromptOverrideRepo,
  ) -> Result<LlmRequest, KeaError>
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn preset_instruction_replaces_mode_template() {
    // upsert preset with instruction "Translate to French"
    // build_llm_request with preset_id = Some("p1"), mode = Improve
    // assert LlmRequest.prompt contains "Translate to French" and source text
}
```

- [ ] **Step 2: Run test — FAIL**

- [ ] **Step 3: Implement**

Resolution order: if `preset_id` set → use preset `instruction` as the user-facing template (append source text); else use `PromptCatalog::rendered` with optional DB override from `PromptOverrideRepo::get(mode)`.

```rust
use kea_engines::LlmRequest;

pub async fn build_llm_request(/* ... */) -> Result<LlmRequest, KeaError> {
    let prompt = /* logic above */;
    Ok(LlmRequest { prompt })
}
```

Add `kea-engines` to `crates/core/Cargo.toml` dependencies.

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(core): build LlmRequest from rewrite preset/mode"
```

---

### Task 11: Extend `ActionRepo` — `finish()` + status updates

**Files:**
- Modify: `crates/core/src/store/actions.rs`, `crates/core/migrations/data/0002_action_finish.sql` (optional — columns exist)

**Interfaces:**
- Produces: `async fn finish(&self, id: i64, status: &str, error: Option<&str>) -> Result<(), KeaError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn finish_marks_action_done() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    run_data_migrations(&pool).await.unwrap();
    let repo = ActionRepo::new(pool);
    let id = repo.record(NewAction {
        feature_id: "rewrite".into(), command: "rewrite".into(),
        engine_id: "openai".into(), model: Some("gpt-4o-mini".into()),
        provider_ref: Some("openai".into()),
    }).await.unwrap();
    repo.finish(id, "ok", None).await.unwrap();
    let rows = repo.recent(1).await.unwrap();
    assert_eq!(rows[0].status, "ok");
}
```

- [ ] **Step 2: Run test — FAIL**

- [ ] **Step 3: Implement**

```rust
pub async fn finish(&self, id: i64, status: &str, error: Option<&str>) -> Result<(), KeaError> {
    sqlx::query("UPDATE actions SET status = ?, error = ?, finished_at = datetime('now') WHERE id = ?")
        .bind(status).bind(error).bind(id)
        .execute(&self.pool).await?;
    Ok(())
}
```

- [ ] **Step 4: Run test — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(core): ActionRepo finish for rewrite audit trail"
```

---

### Task 12: Platform `Hotkeys` trait + `HotkeyBinding` types

**Files:**
- Create: `crates/platform/src/hotkeys.rs`
- Modify: `crates/platform/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub type ActionId = String;
  #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
  pub struct HotkeyBinding { pub accelerator: String } // e.g. "CommandOrControl+Shift+R"
  pub enum HotkeyError { #[error("{0}")] Other(String) }
  #[async_trait]
  pub trait Hotkeys: Send + Sync {
      fn register(&mut self, binding: HotkeyBinding, action: ActionId) -> Result<(), HotkeyError>;
      fn on_action(&self) -> tokio::sync::mpsc::Receiver<ActionId>;
  }
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    struct FakeHotkeys { tx: tokio::sync::mpsc::Sender<ActionId> }
    #[async_trait]
    impl Hotkeys for FakeHotkeys {
        fn register(&mut self, _: HotkeyBinding, action: ActionId) -> Result<(), HotkeyError> {
            self.tx.try_send(action).map_err(|e| HotkeyError::Other(e.to_string()))
        }
        fn on_action(&self) -> tokio::sync::mpsc::Receiver<ActionId> {
            unimplemented!()
        }
    }
    #[test]
    fn binding_type_roundtrips_json() {
        let b = HotkeyBinding { accelerator: "CommandOrControl+Shift+R".into() };
        let json = serde_json::to_string(&b).unwrap();
        let back: HotkeyBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(b, back);
    }
}
```

- [ ] **Step 2–5:** implement trait file, export, test PASS, commit.

```bash
git commit --no-verify -m "feat(platform): Hotkeys trait and HotkeyBinding types"
```

---

### Task 13: Platform `TextIo` trait

**Files:**
- Create: `crates/platform/src/textio.rs`

**Interfaces:**
- Produces:
  ```rust
  pub enum TextIoError { #[error("{0}")] Other(String) }
  #[async_trait]
  pub trait TextIo: Send + Sync {
      async fn capture_selection(&self) -> Result<String, TextIoError>;
      async fn replace(&self, text: &str) -> Result<(), TextIoError>;
  }
  ```

- [ ] **Step 1: Write the failing test** — `FakeTextIo` impl returning canned strings.

- [ ] **Step 2–5:** implement, export from `lib.rs`, commit.

```bash
git commit --no-verify -m "feat(platform): TextIo trait"
```

---

### Task 14: macOS `Hotkeys` implementation (`global-hotkey`)

**Files:**
- Create: `crates/platform/src/macos/mod.rs`, `crates/platform/src/macos/hotkeys.rs`
- Modify: `crates/platform/src/lib.rs`

**Interfaces:**
- Produces: `MacHotkeys` implementing `Hotkeys`; `pub fn new_hotkeys() -> Box<dyn Hotkeys>` with `#[cfg(target_os = "macos")]` branch.

> **Permissions:** global hotkeys require Accessibility on macOS (user grants in System Settings). Unit tests do **not** register real global hotkeys — test accelerator parsing only.

- [ ] **Step 1: Write the failing test (parser only)**

```rust
#[cfg(target_os = "macos")]
mod parse_tests {
    use super::*;
    #[test]
    fn parses_cmd_shift_r() {
        let mods = parse_accelerator("CommandOrControl+Shift+R").unwrap();
        assert!(mods.contains_global_hotkey_shift());
    }
}
```

- [ ] **Step 2: Run test — FAIL**

- [ ] **Step 3: Implement `MacHotkeys`**

Use `global_hotkey::GlobalHotKeyManager`, `hotkey::HotKey`, `global_hotkey::hotkey::{Modifiers, Code}`. Map `CommandOrControl` → `Modifiers::SUPER` on macOS. On register, store `ActionId` in a `HashMap<HotKey, ActionId>`. Install `GlobalHotKeyEvent` listener forwarding to `mpsc::Sender<ActionId>`.

`crates/platform/src/lib.rs`:

```rust
pub mod hotkeys;
pub mod textio;

pub fn new_hotkeys() -> Box<dyn hotkeys::Hotkeys> {
    #[cfg(target_os = "macos")]
    { return Box::new(macos::hotkeys::MacHotkeys::new()); }
    #[cfg(not(target_os = "macos"))]
    { Box::new(stub::StubHotkeys::new()) }
}
```

Add `stub` module returning errors for non-macOS until Tasks 16–18 land.

- [ ] **Step 4: Run parser test — PASS** on macOS; stub compiles on CI Linux.

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(platform): macOS global hotkeys via global-hotkey"
```

---

### Task 15: macOS `TextIo` implementation (`arboard` + `enigo`, D4)

**Files:**
- Create: `crates/platform/src/macos/textio.rs`
- Modify: `crates/platform/src/lib.rs` (`new_text_io()`)

**Interfaces:**
- Produces: `MacTextIo` implementing `TextIo`.

> **Permissions:** Accessibility required for synthetic key events (`enigo`). Manual acceptance verifies; unit tests use a `FakeEnigo`/`FakeClipboard` injected via `TextIo` internals or test-only constructor.

- [ ] **Step 1: Write the failing test (clipboard logic, no real OS)**

```rust
#[test]
fn restore_clipboard_plan_saves_and_restores() {
    let saved = "original";
    let plan = ClipboardPlan::save(saved);
    assert_eq!(plan.restore_value(), saved);
}
```

Implement `ClipboardPlan` struct holding prior clipboard text + optional non-text flag.

- [ ] **Step 2: Run test — FAIL**

- [ ] **Step 3: Implement `MacTextIo`**

```rust
#[async_trait]
impl TextIo for MacTextIo {
    async fn capture_selection(&self) -> Result<String, TextIoError> {
        self.enigo.key_click(enigo::Key::Meta, enigo::Direction::Press);
        self.enigo.key_click(enigo::Key::Unicode('c'), enigo::Direction::Click);
        self.enigo.key_click(enigo::Key::Meta, enigo::Direction::Release);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let text = self.clipboard.get_text().map_err(|e| TextIoError::Other(e.to_string()))?;
        if text.trim().is_empty() {
            return Err(TextIoError::Other("no selection".into()));
        }
        Ok(text)
    }

    async fn replace(&self, text: &str) -> Result<(), TextIoError> {
        let plan = ClipboardPlan::capture(&mut self.clipboard)?;
        self.clipboard.set_text(text).map_err(|e| TextIoError::Other(e.to_string()))?;
        self.paste_synthesized().await?;
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        plan.restore(&mut self.clipboard)?;
        Ok(())
    }
}
```

`new_text_io()` mirrors `new_hotkeys()` with macOS cfg gate.

- [ ] **Step 4: Run unit tests — PASS**

- [ ] **Step 5: Commit**

```bash
git commit --no-verify -m "feat(platform): macOS TextIo clipboard capture and synthetic paste"
```

---

### Task 16: **[PARALLEL — Windows]** `Hotkeys` implementation

**Files:**
- Create: `crates/platform/src/windows/hotkeys.rs`
- Modify: `crates/platform/src/lib.rs` (`#[cfg(target_os = "windows")]`)

**Interfaces:**
- Produces: `WindowsHotkeys` via `global-hotkey` (`RegisterHotKey` backend).

- [ ] **Step 1:** accelerator parse test (same as Task 14).
- [ ] **Step 2–5:** implement `WindowsHotkeys`, wire `new_hotkeys()`, commit.

```bash
git commit --no-verify -m "feat(platform): Windows global hotkeys"
```

---

### Task 17: **[PARALLEL — Windows]** `TextIo` implementation

**Files:**
- Create: `crates/platform/src/windows/textio.rs`

**Interfaces:**
- Produces: `WindowsTextIo` — `Ctrl+C` / `Ctrl+V` via `enigo`; `arboard` clipboard.

- [ ] **TDD steps** mirror Task 15 with `Control` modifier; commit.

```bash
git commit --no-verify -m "feat(platform): Windows TextIo clipboard replace"
```

---

### Task 18: **[PARALLEL — Linux X11]** hotkeys + textio

**Files:**
- Create: `crates/platform/src/linux/hotkeys_x11.rs`, `crates/platform/src/linux/textio.rs`

**Interfaces:**
- Produces: X11 backends via `global-hotkey` + `enigo` XTest.

- [ ] **TDD:** parser tests + `FakeTextIo` integration; runtime selects X11 when `WAYLAND_DISPLAY` unset.

```bash
git commit --no-verify -m "feat(platform): Linux X11 hotkeys and TextIo"
```

---

### Task 19: **[PARALLEL — Linux Wayland]** portal hotkeys + `uinput` paste

**Files:**
- Create: `crates/platform/src/linux/hotkeys_wayland.rs`, extend `linux/textio.rs`

**Interfaces:**
- Produces: `WaylandHotkeys` using `ashpd` / `global-shortcuts` portal; `WaylandTextIo` using `uinput` evdev for synthetic Ctrl+V.

> **Permissions:** Wayland portal consent dialog; `uinput` group membership documented in `docs/cross-platform/plans/CONTRACTS.md`. CI does not exercise portal — manual GNOME/KDE checklist only.

- [ ] **Step 1:** unit-test portal URL / accelerator serialization (no portal in CI).
- [ ] **Step 2–5:** implement behind `WAYLAND_DISPLAY` detection in `new_hotkeys()` / `new_text_io()`.

```bash
git commit --no-verify -m "feat(platform): Linux Wayland hotkeys portal and uinput TextIo"
```

---

### Task 20: `HotkeyBindingRepo` (config.db)

**Files:**
- Create: `crates/core/src/store/hotkeys.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct HotkeyBindingRow { pub feature_id: String, pub command: String, pub accelerator: String }
  pub struct HotkeyBindingRepo { /* get, set, list */ }
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn set_rewrite_hotkey() {
    let pool = open_pool("sqlite::memory:").await.unwrap();
    run_config_migrations(&pool).await.unwrap();
    let repo = HotkeyBindingRepo::new(pool);
    repo.set("rewrite", "rewrite", "CommandOrControl+Shift+R").await.unwrap();
    let row = repo.get("rewrite", "rewrite").await.unwrap().unwrap();
    assert_eq!(row.accelerator, "CommandOrControl+Shift+R");
}
```

- [ ] **Step 2–5:** implement against `hotkey_bindings` table; commit.

```bash
git commit --no-verify -m "feat(core): HotkeyBindingRepo on config.db"
```

---

### Task 21: Extend `Feature` trait — `commands()` + `RewriteFeature`

**Files:**
- Modify: `crates/features/src/feature.rs`, `crates/features/src/demo.rs`
- Create: `crates/features/src/rewrite.rs`
- Modify: `crates/features/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct FeatureCommand { pub name: &'static str, pub default_hotkey: Option<&'static str> }
  pub trait Feature: Send + Sync {
      fn id(&self) -> &str;
      fn required_caps(&self) -> Vec<CapSlot>;
      fn commands(&self) -> Vec<FeatureCommand> { vec![] }
  }
  pub struct RewriteFeature;
  // impl Feature — id "rewrite", llm slot, commands: [{ name: "rewrite", default_hotkey: Some("CommandOrControl+Shift+R") }]
  ```

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn rewrite_declares_llm_slot_and_command() {
    let f = RewriteFeature;
    assert_eq!(f.id(), "rewrite");
    assert_eq!(f.required_caps()[0].name, "llm");
    assert_eq!(f.commands()[0].name, "rewrite");
}
```

- [ ] **Step 2–5:** extend trait (default `commands()` empty so `DemoFeature` unchanged), implement `RewriteFeature`, commit.

```bash
git commit --no-verify -m "feat(features): RewriteFeature with llm slot and command"
```

---

### Task 22: `run_rewrite()` orchestration

**Files:**
- Modify: `crates/features/src/rewrite.rs`
- Modify: `crates/features/Cargo.toml` (deps: `kea-core`, `kea-engines`, `kea-platform`)

**Interfaces:**
- Consumes: `EngineRegistry`, `SlotResolver`, `BindingRepo`, `ActionRepo`, `TextIo`, `RewriteInput`, pools.
- Produces:
  ```rust
  pub async fn run_rewrite(
      engines: &EngineRegistry,
      bindings: &BindingRepo,
      actions: &ActionRepo,
      presets: &PresetRepo,
      overrides: &PromptOverrideRepo,
      textio: &dyn TextIo,
      input: RewriteInput,
  ) -> Result<String, String>
  ```

- [ ] **Step 1: Write the failing test with fakes**

```rust
#[tokio::test]
async fn run_rewrite_calls_llm_and_textio() {
    let mut reg = EngineRegistry::default();
    reg.register_llm(Arc::new(NoopLlmEngine));
    let textio = Arc::new(FakeTextIo { selection: "bad text".into() });
    // SlotResolver auto-binds noop
    let pool = open_pool("sqlite::memory:").await.unwrap();
    run_config_migrations(&pool).await.unwrap();
    run_data_migrations(&pool).await.unwrap();
    let bindings = BindingRepo::new(pool.clone());
    let actions = ActionRepo::new(pool);
    let presets = PresetRepo::new(pool.clone());
    let overrides = PromptOverrideRepo::new(pool);
    let out = run_rewrite(
        &reg, &bindings, &actions, &presets, &overrides,
        textio.as_ref(),
        RewriteInput { source_text: String::new(), mode: RewriteMode::Improve, preset_id: None, custom_instruction: None },
    ).await.unwrap();
    assert!(out.contains("echo:"));
    assert_eq!(textio.replaced.lock().unwrap().as_deref(), Some(out.as_str()));
}
```

`FakeTextIo`: `capture_selection` returns `selection`; `replace` stores text.

Flow inside `run_rewrite`:
1. `textio.capture_selection()` if `input.source_text` empty.
2. `SlotResolver::resolve_llm("rewrite", "llm")` → engine id; load `Binding` for model/provider_ref.
3. `build_llm_request(...)`.
4. `actions.record(NewAction { feature_id: "rewrite", command: "rewrite", ... })`.
5. `engine.complete(llm_req).await`.
6. `textio.replace(&response.text).await`.
7. `actions.finish(id, "ok", None)`.

- [ ] **Step 2–5:** implement, test PASS, commit.

```bash
git commit --no-verify -m "feat(features): run_rewrite orchestration with action logging"
```

---

### Task 23: Tauri `AppState` expansion + engine registration

**Files:**
- Modify: `src-tauri/src/main.rs`, `src-tauri/Cargo.toml`

**Interfaces:**
- Produces: `AppState` fields:
  ```rust
  struct AppState {
      engines: EngineRegistry,
      features: FeatureRegistry,
      config_pool: SqlitePool,
      data_pool: SqlitePool,
      credentials: Arc<dyn CredentialStore>,
      hotkeys: Mutex<Box<dyn Hotkeys>>,
  }
  ```

- [ ] **Step 1: Write the failing test** in `src-tauri/src/commands.rs`:

```rust
#[test]
fn phase1_engine_ids_include_openai() {
    // build registry via register_phase1_engines with in-memory deps
    // assert list contains openai
}
```

- [ ] **Step 2–5:** in `setup`:
  - Open `data_pool` (already migrated) and store in `AppState`.
  - `credentials = Arc::new(KeyringCredentialStore::new("ai.kea.app"))` (dev: `InMemoryCredentialStore` behind `cfg(debug_assertions)` optional).
  - `register_phase1_engines(...)`.
  - `features.register(Arc::new(RewriteFeature))`.
  - `hotkeys: Mutex::new(new_hotkeys())`.

```bash
git commit --no-verify -m "feat(app): AppState with phase1 engines, credentials, hotkeys"
```

---

### Task 24: Tauri commands — providers, presets, bindings, rewrite

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/main.rs`

**Interfaces:**
- Produces commands:
  ```rust
  #[tauri::command] async fn list_llm_engines(state) -> Vec<EngineInfoDto>
  #[tauri::command] async fn get_provider_config(state, provider_ref: String) -> Option<ProviderConfig>
  #[tauri::command] async fn set_provider_config(state, provider_ref: String, cfg: ProviderConfig) -> Result<(), String>
  #[tauri::command] async fn set_credential(state, provider_ref: String, secret: String) -> Result<(), String>
  #[tauri::command] async fn delete_credential(state, provider_ref: String) -> Result<(), String>
  #[tauri::command] async fn list_presets(state) -> Vec<RewritePreset>
  #[tauri::command] async fn upsert_preset(state, preset: RewritePreset) -> Result<(), String>
  #[tauri::command] async fn set_binding_full(state, feature, slot, engine, model, provider_ref) -> Result<(), String>
  #[tauri::command] async fn get_hotkey(state, feature, command) -> Option<String>
  #[tauri::command] async fn set_hotkey(state, feature, command, accelerator) -> Result<(), String>
  #[tauri::command] async fn run_rewrite_cmd(state, mode: RewriteMode, preset_id: Option<String>, custom_instruction: Option<String>) -> Result<String, String>
  ```

- [ ] **Step 1: Write failing test** for `engine_info` pure helper mapping `EngineCaps`.

- [ ] **Step 2–5:** implement commands; extend `set_binding` to accept optional `model` + `provider_ref`; register in `generate_handler!`; commit.

```bash
git commit --no-verify -m "feat(app): Tauri commands for rewrite providers, presets, and run"
```

---

### Task 25: Tauri events + hotkey dispatcher

**Files:**
- Create: `src-tauri/src/events.rs`
- Modify: `src-tauri/src/main.rs`

**Interfaces:**
- Produces:
  ```rust
  pub fn emit_rewrite_progress(app: &AppHandle, message: &str)
  pub fn emit_rewrite_error(app: &AppHandle, message: &str)
  ```
  Events: `rewrite:progress`, `rewrite:error` (payload `{ message: string }`).

- [ ] **Step 1: Write failing test** for payload serde.

- [ ] **Step 2–5:** in `setup`, spawn task:

```rust
let mut hotkeys = state.hotkeys.lock().unwrap();
let repo = HotkeyBindingRepo::new(config_pool.clone());
if let Some(row) = repo.get("rewrite", "rewrite").await? {
    hotkeys.register(HotkeyBinding { accelerator: row.accelerator }, "rewrite:rewrite".into())?;
}
let mut rx = hotkeys.on_action();
let app_handle = app.handle().clone();
tauri::async_runtime::spawn(async move {
    while let Some(action) = rx.recv().await {
        if action == "rewrite:rewrite" {
            emit_rewrite_progress(&app_handle, "Capturing selection...");
            let result = run_rewrite_cmd(/* state clone */).await;
            match result {
                Ok(_) => emit_rewrite_progress(&app_handle, "Done"),
                Err(e) => emit_rewrite_error(&app_handle, &e),
            }
        }
    }
});
```

Use `AppHandle` + `State` managed via `Arc<AppState>` for the hotkey task.

```bash
git commit --no-verify -m "feat(app): rewrite hotkey dispatcher and progress events"
```

---

### Task 26: UI routing shell + extended `api.ts`

**Files:**
- Modify: `ui/src/App.tsx`, `ui/src/api.ts`
- Create: `ui/src/pages/ConfigurationPage.tsx`, `ui/src/pages/FeaturesPage.tsx`

**Interfaces:**
- Produces: top-level nav **Configuration** | **Features**; typed API wrappers for all Task 24 commands.

- [ ] **Step 1: Write the failing check**

Add to `ui/src/api.ts`:

```ts
export type ProviderConfig = { base_url: string; default_model: string };
export const getProviderConfig = (providerRef: string) =>
  invoke<ProviderConfig | null>("get_provider_config", { providerRef });
```

Run: `cd ui && npm run typecheck`
Expected: FAIL — `ConfigurationPage` not found.

- [ ] **Step 2–5:** add minimal pages, router tabs in `App.tsx`, full `api.ts` exports, `npm run build` PASS, commit.

```bash
git commit --no-verify -m "feat(ui): routing shell and typed Phase 1 API"
```

---

### Task 27: UI `CredentialField` + `EngineConfig` components

**Files:**
- Create: `ui/src/components/CredentialField.tsx`, `ui/src/components/EngineConfig.tsx`
- Modify: `ui/src/pages/ConfigurationPage.tsx`

**Interfaces:**
- Consumes: `getProviderConfig`, `setProviderConfig`, `setCredential`, `deleteCredential`.
- Produces: form per `provider_ref` (`openai`, `local-llm`) — base URL, model, API key (password input; never display stored key).

- [ ] **Step 1: Typecheck failing** — import components in `ConfigurationPage`.

- [ ] **Step 2–5:** implement components (controlled inputs, save buttons), wire page, commit.

```bash
git commit --no-verify -m "feat(ui): EngineConfig and CredentialField on Configuration page"
```

---

### Task 28: UI `SlotBinder` + `HotkeyBinder` components

**Files:**
- Create: `ui/src/components/SlotBinder.tsx`, `ui/src/components/HotkeyBinder.tsx`
- Modify: `ui/src/pages/FeaturesPage.tsx`

**Interfaces:**
- Consumes: `listEngines`, `getBinding`, `setBinding` (extended with model/provider_ref), `getHotkey`, `setHotkey`.
- Produces: rewrite feature panel — pick `llm` engine (`openai` | `openai-compatible`), model text field, provider_ref selector; hotkey capture field.

- [ ] **TDD gate:** `npm run typecheck` + manual render in `FeaturesPage`.

```bash
git commit --no-verify -m "feat(ui): SlotBinder and HotkeyBinder for rewrite feature"
```

---

### Task 29: UI `SettingsForm` — rewrite mode + presets

**Files:**
- Create: `ui/src/components/SettingsForm.tsx`
- Modify: `ui/src/pages/FeaturesPage.tsx`

**Interfaces:**
- Consumes: `listPresets`, `upsertPreset`, `getSetting`/`setSetting` for `rewrite.active_mode` and `rewrite.active_preset_id`.
- Produces: mode `<select>` (`RewriteMode` values), preset picker, optional custom instruction textarea (shown for `ask_kea`).

- [ ] **Step 1:** typecheck fails without component.
- [ ] **Step 2–5:** implement, wire "Run rewrite" test button calling `runRewriteCmd`, commit.

```bash
git commit --no-verify -m "feat(ui): SettingsForm for rewrite mode and presets"
```

---

### Task 30: Listen for `rewrite:progress` / `rewrite:error` in UI

**Files:**
- Create: `ui/src/components/StatusPill.tsx`
- Modify: `ui/src/App.tsx`

**Interfaces:**
- Consumes: `@tauri-apps/api/event` `listen("rewrite:progress")`, `listen("rewrite:error")`.
- Produces: transient status pill at bottom of window.

- [ ] **Step 1:** add listener in `App.tsx`, verify typecheck.
- [ ] **Step 2–5:** implement `StatusPill`, manual `cargo tauri dev` smoke, commit.

```bash
git commit --no-verify -m "feat(ui): StatusPill listening for rewrite events"
```

---

### Task 31: End-to-end acceptance (macOS manual + CI compile)

**Files:** none (verification only)

- [ ] **Step 1: CI**

Run: `cargo test --workspace && cargo build -p kea-app && (cd ui && npm run build)`
Expected: PASS on all three OS matrix jobs.

- [ ] **Step 2: macOS manual checklist**

1. `cargo tauri dev`
2. Configuration → set OpenAI base URL `https://api.openai.com/v1`, model `gpt-4o-mini`, save API key to keyring.
3. Features → bind rewrite `llm` slot to `openai`; set hotkey `Command+Shift+R`.
4. Select text in TextEdit → press hotkey → selection replaced; `data.db` `actions` row with `feature_id=rewrite`, `status=ok`.
5. Disconnect network → UI shows `rewrite:error` (engine failure), action `status=error`.

- [ ] **Step 3: Document deferred platform checks**

Windows/Linux/Wayland manual checks land when Tasks 16–19 complete.

---

## Phase 1 Definition of Done

- `cargo test --workspace` green; `cargo build -p kea-app` succeeds; `ui` builds on CI (macOS, Windows, Linux).
- **macOS:** global hotkey → capture selection → `LlmEngine::complete` → in-place replace (D4) works against a real OpenAI or compatible provider configured in UI.
- `openai` and `openai-compatible` engines registered; credentials in keyring; provider base URL + model in `config.db` only.
- Rewrite modes + preset/prompt catalog persisted in `config.db`; `RewriteRequest` builder produces `LlmRequest`.
- `RewriteFeature` declares `llm` `CapSlot`; `run_rewrite` records actions in `data.db`.
- Tauri commands expose provider/preset/binding/hotkey management; `rewrite:progress` / `rewrite:error` events emitted.
- UI: **Configuration** page (`EngineConfig`, `CredentialField`) and **Features** page (`SlotBinder`, `HotkeyBinder`, `SettingsForm`) functional.
- Unit tests use `wiremock` / injected `HttpClient` — no CI network calls to OpenAI.
- Windows/Linux/Wayland platform impls (Tasks 16–19) may trail macOS E2E but must not break the workspace build.

## Self-Review (spec coverage map)

| Spec reference | Plan tasks |
|----------------|------------|
| §3 D1 plugin model | Tasks 7, 20–22 (registries + rewrite feature) |
| §3 D4 clipboard + synthetic paste | Task 15 (macOS), 17–19 (other OS) |
| §3 D9 config.db / keyring boundary | Tasks 2–3, 11, 23–24 |
| §3 D10 React UI | Tasks 26–30 |
| §4.1 `LlmEngine::complete` | Tasks 4–7 |
| §4.2 `Hotkeys` / `TextIo` traits | Tasks 12–19 |
| §4.3 Feature plugin + slots | Tasks 21–22 |
| §4.4 slot resolution | Task 22 (uses existing `SlotResolver::resolve_llm`) |
| §5 Rewrite data flow (6 steps) | Tasks 15, 22, 25 |
| §6.1 presets + prompt catalog in config.db | Tasks 2, 8–10 |
| §6.3 actions audit trail | Tasks 11, 22 |
| §7 UI component library | Tasks 27–29 |
| §8 integration matrix | Tasks 14–19 |
| §9 Phase 1 outcome | Definition of Done |

### Deferred to later phases (explicit boundaries)

- **Dictation / STT engines / `platform/audio`** — Phase 2.
- **Meetings, loopback audio, meeting tables** — Phase 3.
- **Parakeet, Whisper, TTS, `ModelManager`, local model download** — Phase 4.
- **macOS Accessibility insertion path (D12)** — Phase 4 enhancement alongside D4 baseline.
- **History / Logs pages, conversation persistence, retention pruning** — Phase 4 (actions recorded in Phase 1; full History UI deferred).
- **Overlay window / speech level meter** — Phase 2 (Phase 1 uses `StatusPill` in main window only).
- **NSServices, notarized installers, autostart, first-run permission wizards** — Phase 4.
- **`audio_refinement` mode** — catalog entry ships in Phase 1; automatic post-dictation pass wires in Phase 2.

---

## PLAN CORRECTION (2026-06-25, post Stage A) — break the core↔engines cycle

Tasks 3/5/6/7 as originally written make `kea-engines` depend on `kea-core`
(`CredentialStore`, `ProviderConfigRepo`). But `kea-core` already depends on
`kea-engines` (`SlotResolver` uses `EngineRegistry`) → **dependency cycle, which
Cargo rejects.** Resolve via **dependency inversion**:

**New seam — `crates/engines/src/provider.rs`** (no external deps beyond async-trait/serde):

```rust
use async_trait::async_trait;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ProviderConfig { pub base_url: String, pub default_model: String }

#[async_trait]
pub trait CredentialSource: Send + Sync {
    /// Returns the API key/secret for a provider_ref, if set.
    async fn api_key(&self, provider_ref: &str) -> Option<String>;
}

#[async_trait]
pub trait ProviderConfigSource: Send + Sync {
    /// Returns the base_url + default_model for a provider_ref, if configured.
    async fn config(&self, provider_ref: &str) -> Option<ProviderConfig>;
}
```
Re-export from `crates/engines/src/lib.rs`: `pub use provider::{ProviderConfig, CredentialSource, ProviderConfigSource};`

**Engine structs** (Tasks 5/6) hold `Arc<dyn CredentialSource>` + `Arc<dyn ProviderConfigSource>`
(both from `kea_engines::provider`) + `Arc<dyn HttpClient>` + a `provider_ref: String`.
They do NOT import `kea_core`. Unit tests provide in-engine fakes of the two
traits + `wiremock`.

**`kea-core` (Task 3)** keeps `ProviderConfigRepo` (config.db, settings-backed,
NO secrets) but ALSO implements `kea_engines::ProviderConfigSource` for it, and
provides a `CredentialSourceAdapter` that implements `kea_engines::CredentialSource`
over the existing `CredentialStore` (keyring). core→engines is allowed.

**Wiring (Task 23)** constructs the engines passing core's implementations of the
two engine traits. This is the only correction; all other task code stands.
