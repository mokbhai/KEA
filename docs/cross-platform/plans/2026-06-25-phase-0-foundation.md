# KEA Phase 0 — Foundation + Plugin Framework Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the KEA Tauri 2.x + Rust workspace with the three-layer plugin framework (Feature / Engine / Platform registries + slot resolution), the SQLite data layer (`config.db` + `data.db`), keyring-backed `CredentialStore`, tracing logs, and a React + tray shell — proven end-to-end by one trivial demo feature resolving one trivial engine, building in CI on macOS/Windows/Linux.

**Architecture:** A Cargo workspace (`crates/core`, `crates/engines`, `crates/features`, `crates/platform`, `crates/infer`) wired into a thin `src-tauri` shell, with a `ui/` React app. Consumers depend on traits, never concretes. `core` owns settings, secrets abstraction, the SQLite stores, logging, and slot resolution. Phase 0 ships no real feature — a `demo` feature + `noop` engine exist only to exercise registration → binding → resolution → run across the IPC boundary.

**Tech Stack:** Rust (edition 2021), Tauri 2.x, `sqlx` (SQLite, `runtime-tokio`), `tokio`, `async-trait`, `serde`/`serde_json`, `tracing` + `tracing-subscriber` + `tracing-appender`, `keyring`, `thiserror`, Vite + React + TypeScript.

## Global Constraints

- **Product name:** `KEA` everywhere (Cargo package names `kea-*`, Tauri `productName: "KEA"`, bundle id `ai.kea.app`). _(D13 — name fixed regardless of `kea` collisions.)_
- **Plugin model:** internal trait + registry, compiled in. No dynamic/dylib/WASM loading. _(D1, D2.)_
- **Storage boundary (D9):** `config.db` = settings/presets/prompt-catalog/bindings; `data.db` = actions/conversations/meetings/runtime state; **keyring = credentials only**. The DBs store references (`engine_id`, model, `provider_ref`), **never** API keys, provider base URLs, or settings values. No plaintext keys in any DB or file.
- **DB engine:** SQLite via `sqlx` with versioned migrations run on startup. Two separate database files. Paths resolved via Tauri `path` APIs.
- **Async:** all engine/feature/platform trait methods that do I/O are `async` (`async-trait`).
- **TDD:** every code task is test-first. Rust async tests use `#[tokio::test]`. Store tests use in-memory SQLite (`sqlite::memory:`) unless a file path is under test (use `tempfile`).
- **Targets:** macOS, Windows, Linux (X11 + Wayland) — code must compile on all three; CI builds all three.
- **Commits:** frequent, conventional-commit messages, one per task minimum.

---

## File Structure

```
kea/
├─ Cargo.toml                      # workspace manifest (members + shared deps)
├─ crates/
│  ├─ core/
│  │  ├─ Cargo.toml
│  │  └─ src/
│  │     ├─ lib.rs                 # re-exports; KeaError
│  │     ├─ error.rs               # KeaError (thiserror)
│  │     ├─ store/
│  │     │  ├─ mod.rs              # Store: owns config + data pools
│  │     │  ├─ db.rs              # open_pool, run_migrations
│  │     │  ├─ settings.rs        # SettingsRepo (config.db)
│  │     │  ├─ bindings.rs        # BindingRepo (config.db)
│  │     │  └─ actions.rs         # ActionRepo (data.db)
│  │     ├─ secrets.rs            # CredentialStore trait + InMemory + Keyring impls
│  │     ├─ log.rs                # init_logging
│  │     └─ resolve.rs            # SlotResolver (uses EngineRegistry + BindingRepo)
│  │  └─ migrations/
│  │     ├─ config/0001_init.sql
│  │     └─ data/0001_init.sql
│  ├─ engines/
│  │  ├─ Cargo.toml
│  │  └─ src/
│  │     ├─ lib.rs
│  │     ├─ traits.rs             # SttEngine, TtsEngine, LlmEngine, EngineCaps, requests/responses
│  │     ├─ registry.rs           # EngineRegistry
│  │     └─ noop.rs               # NoopLlmEngine (demo)
│  ├─ features/
│  │  ├─ Cargo.toml
│  │  └─ src/
│  │     ├─ lib.rs
│  │     ├─ feature.rs            # Feature trait, CapSlot, FeatureCtx
│  │     ├─ registry.rs           # FeatureRegistry
│  │     └─ demo.rs               # DemoFeature (resolves one LLM slot)
│  ├─ platform/
│  │  ├─ Cargo.toml
│  │  └─ src/lib.rs               # trait stubs (Hotkeys/TextIo/AudioIo/Permissions) — defined, not impl'd in P0
│  └─ infer/
│     ├─ Cargo.toml
│     └─ src/lib.rs               # empty placeholder (real plumbing in Phase 2/3)
├─ src-tauri/
│  ├─ Cargo.toml
│  ├─ tauri.conf.json             # KEA identity, bundle id, tray
│  ├─ build.rs
│  └─ src/
│     ├─ main.rs                  # builder, state, tray, migrations on setup
│     └─ commands.rs              # list_engines/list_features/get_setting/set_setting/get_binding/set_binding/run_demo
└─ ui/
   ├─ package.json
   ├─ vite.config.ts
   ├─ index.html
   └─ src/
      ├─ main.tsx
      ├─ api.ts                   # typed wrappers over Tauri invoke
      └─ App.tsx                  # minimal Settings page
```

---

### Task 1: Workspace scaffold + Tauri shell with KEA identity

**Files:**
- Create: `Cargo.toml` (workspace), `crates/core/Cargo.toml`, `crates/core/src/lib.rs`, `crates/engines/Cargo.toml`, `crates/engines/src/lib.rs`, `crates/features/Cargo.toml`, `crates/features/src/lib.rs`, `crates/platform/Cargo.toml`, `crates/platform/src/lib.rs`, `crates/infer/Cargo.toml`, `crates/infer/src/lib.rs`
- Create: `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/build.rs`, `src-tauri/src/main.rs`
- Test: `crates/core/src/lib.rs` (inline `#[cfg(test)]`)

**Interfaces:**
- Produces: workspace that `cargo build` and `cargo test` succeed on; crate names `kea-core`, `kea-engines`, `kea-features`, `kea-platform`, `kea-infer`, `kea-app` (src-tauri).

- [ ] **Step 1: Write the failing test**

In `crates/core/src/lib.rs`:

```rust
pub fn crate_name() -> &'static str {
    "kea-core"
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_the_crate() {
        assert_eq!(crate_name(), "kea-core");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-core`
Expected: FAIL — `error: failed to load manifest` / package not found (workspace not yet defined).

- [ ] **Step 3: Write the workspace + crate manifests**

Root `Cargo.toml`:

```toml
[workspace]
resolver = "2"
members = ["crates/core", "crates/engines", "crates/features", "crates/platform", "crates/infer", "src-tauri"]

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sqlx = { version = "0.8", features = ["sqlite", "runtime-tokio", "macros", "migrate"] }
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
tracing-appender = "0.2"
keyring = "3"
tempfile = "3"
```

`crates/core/Cargo.toml`:

```toml
[package]
name = "kea-core"
version = "0.1.0"
edition = "2021"

[dependencies]
kea-engines = { path = "../engines" }
tokio.workspace = true
async-trait.workspace = true
serde.workspace = true
serde_json.workspace = true
sqlx.workspace = true
thiserror.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
tracing-appender.workspace = true
keyring.workspace = true

[dev-dependencies]
tempfile.workspace = true
```

`crates/engines/Cargo.toml` and `crates/features/Cargo.toml` follow the same shape (features depends on engines); `crates/platform/Cargo.toml` and `crates/infer/Cargo.toml` need only `async-trait`, `serde`, `thiserror`. Each gets a minimal `src/lib.rs`:

```rust
// engines/features/platform/infer src/lib.rs (placeholder until later tasks)
```

(For features: `pub fn crate_name() -> &'static str { "kea-features" }` analogously, so each crate has a smoke target.)

- [ ] **Step 4: Add the Tauri shell crate**

`src-tauri/Cargo.toml`:

```toml
[package]
name = "kea-app"
version = "0.1.0"
edition = "2021"

[build-dependencies]
tauri-build = { version = "2", features = [] }

[dependencies]
kea-core = { path = "../crates/core" }
kea-engines = { path = "../crates/engines" }
kea-features = { path = "../crates/features" }
tauri = { version = "2", features = ["tray-icon"] }
tokio.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
```

`src-tauri/build.rs`:

```rust
fn main() {
    tauri_build::build();
}
```

`src-tauri/tauri.conf.json`:

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "KEA",
  "version": "0.1.0",
  "identifier": "ai.kea.app",
  "build": { "frontendDist": "../ui/dist", "devUrl": "http://localhost:5173" },
  "app": {
    "windows": [{ "title": "KEA", "width": 900, "height": 640, "visible": false }],
    "trayIcon": { "id": "main", "iconPath": "icons/icon.png" }
  },
  "bundle": { "active": true, "targets": "all", "icon": ["icons/icon.png"] }
}
```

`src-tauri/src/main.rs` (minimal; tray + commands wired in Task 11):

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running KEA");
}
```

Add a placeholder `src-tauri/icons/icon.png` (any 512×512 PNG) so the bundler resolves.

- [ ] **Step 5: Run tests to verify pass + workspace builds**

Run: `cargo test --workspace`
Expected: PASS (core + features smoke tests pass; all crates compile).

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml crates src-tauri
git commit -m "feat(kea): scaffold Cargo workspace + Tauri shell with KEA identity"
```

---

### Task 2: `config.db` bootstrap + migration runner

**Files:**
- Create: `crates/core/src/error.rs`, `crates/core/src/store/mod.rs`, `crates/core/src/store/db.rs`, `crates/core/migrations/config/0001_init.sql`
- Modify: `crates/core/src/lib.rs` (add `pub mod error; pub mod store;`)
- Test: `crates/core/src/store/db.rs` (inline tests)

**Interfaces:**
- Produces: `KeaError`; `pub async fn open_pool(url: &str) -> Result<SqlitePool, KeaError>`; `pub async fn run_config_migrations(pool: &SqlitePool) -> Result<(), KeaError>`.

- [ ] **Step 1: Write the failing test**

In `crates/core/src/store/db.rs`:

```rust
use sqlx::SqlitePool;
use crate::error::KeaError;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_create_settings_table() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        // settings table exists -> this query succeeds (0 rows)
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM settings")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(count, 0);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-core store::db`
Expected: FAIL — `open_pool` / `run_config_migrations` not found.

- [ ] **Step 3: Write the migration + implementation**

`crates/core/migrations/config/0001_init.sql`:

```sql
CREATE TABLE settings (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL          -- JSON-encoded value
);

CREATE TABLE bindings (
    feature_id   TEXT NOT NULL,
    slot         TEXT NOT NULL,
    engine_id    TEXT NOT NULL,
    model        TEXT,
    provider_ref TEXT,
    PRIMARY KEY (feature_id, slot)
);
```

`crates/core/src/error.rs`:

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum KeaError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}
```

`crates/core/src/store/db.rs` (above the test module):

```rust
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use crate::error::KeaError;

pub async fn open_pool(url: &str) -> Result<SqlitePool, KeaError> {
    Ok(SqlitePoolOptions::new()
        .max_connections(4)
        .connect(url)
        .await?)
}

static CONFIG_MIGRATOR: sqlx::migrate::Migrator =
    sqlx::migrate!("./migrations/config");

pub async fn run_config_migrations(pool: &SqlitePool) -> Result<(), KeaError> {
    CONFIG_MIGRATOR.run(pool).await?;
    Ok(())
}
```

`crates/core/src/store/mod.rs`:

```rust
pub mod db;
pub mod settings;
pub mod bindings;
pub mod actions;
```

Add to `crates/core/src/lib.rs`: `pub mod error; pub mod store;`. (Create empty `settings.rs`, `bindings.rs`, `actions.rs` with `// filled in later tasks` so `mod.rs` compiles.)

> Note: opening a file-backed SQLite that doesn't exist yet needs `?mode=rwc` in the URL; `sqlite::memory:` needs none. The runtime path is built in Task 11.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-core store::db`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core
git commit -m "feat(core): config.db pool + migration runner"
```

---

### Task 3: Settings repository (`config.db`)

**Files:**
- Create/replace: `crates/core/src/store/settings.rs`
- Test: same file (inline)

**Interfaces:**
- Consumes: `open_pool`, `run_config_migrations`.
- Produces: `pub struct SettingsRepo { pool: SqlitePool }`; `SettingsRepo::new(pool)`; `async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, KeaError>`; `async fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), KeaError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn set_then_get_roundtrips() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = SettingsRepo::new(pool);

        repo.set("log_level", &"debug".to_string()).await.unwrap();
        let got: Option<String> = repo.get("log_level").await.unwrap();
        assert_eq!(got, Some("debug".to_string()));

        let missing: Option<String> = repo.get("nope").await.unwrap();
        assert_eq!(missing, None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-core store::settings`
Expected: FAIL — `SettingsRepo` not found.

- [ ] **Step 3: Write the implementation**

```rust
use serde::{de::DeserializeOwned, Serialize};
use sqlx::SqlitePool;
use crate::error::KeaError;

pub struct SettingsRepo { pool: SqlitePool }

impl SettingsRepo {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub async fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, KeaError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT value FROM settings WHERE key = ?")
                .bind(key).fetch_optional(&self.pool).await?;
        match row {
            Some((json,)) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    pub async fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), KeaError> {
        let json = serde_json::to_string(value)?;
        sqlx::query("INSERT INTO settings(key, value) VALUES(?, ?)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(key).bind(json).execute(&self.pool).await?;
        Ok(())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-core store::settings`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/store/settings.rs
git commit -m "feat(core): typed settings repository on config.db"
```

---

### Task 4: `data.db` bootstrap + actions repository

**Files:**
- Create: `crates/core/migrations/data/0001_init.sql`
- Modify: `crates/core/src/store/db.rs` (add `run_data_migrations`)
- Create/replace: `crates/core/src/store/actions.rs`
- Test: `crates/core/src/store/actions.rs` (inline)

**Interfaces:**
- Consumes: `open_pool`.
- Produces: `pub async fn run_data_migrations(pool: &SqlitePool) -> Result<(), KeaError>`; `pub struct ActionRepo`; `NewAction { feature_id, command, engine_id, model, provider_ref }`; `async fn record(&self, a: NewAction) -> Result<i64, KeaError>`; `async fn recent(&self, limit: i64) -> Result<Vec<ActionRow>, KeaError>`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_data_migrations};

    #[tokio::test]
    async fn record_then_list() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        let repo = ActionRepo::new(pool);

        let id = repo.record(NewAction {
            feature_id: "demo".into(), command: "ping".into(),
            engine_id: "noop".into(), model: None, provider_ref: None,
        }).await.unwrap();
        assert!(id > 0);

        let rows = repo.recent(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature_id, "demo");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-core store::actions`
Expected: FAIL — `ActionRepo` / `run_data_migrations` not found.

- [ ] **Step 3: Write migration + implementation**

`crates/core/migrations/data/0001_init.sql`:

```sql
CREATE TABLE actions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    feature_id   TEXT NOT NULL,
    command      TEXT NOT NULL,
    engine_id    TEXT NOT NULL,
    model        TEXT,
    provider_ref TEXT,
    started_at   TEXT NOT NULL DEFAULT (datetime('now')),
    finished_at  TEXT,
    status       TEXT NOT NULL DEFAULT 'started',
    error        TEXT
);
```

Add to `crates/core/src/store/db.rs`:

```rust
static DATA_MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations/data");

pub async fn run_data_migrations(pool: &SqlitePool) -> Result<(), KeaError> {
    DATA_MIGRATOR.run(pool).await?;
    Ok(())
}
```

`crates/core/src/store/actions.rs`:

```rust
use sqlx::SqlitePool;
use crate::error::KeaError;

pub struct ActionRepo { pool: SqlitePool }

pub struct NewAction {
    pub feature_id: String,
    pub command: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
}

#[derive(serde::Serialize)]
pub struct ActionRow {
    pub id: i64,
    pub feature_id: String,
    pub command: String,
    pub engine_id: String,
    pub status: String,
}

impl ActionRepo {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub async fn record(&self, a: NewAction) -> Result<i64, KeaError> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO actions(feature_id, command, engine_id, model, provider_ref)
             VALUES(?, ?, ?, ?, ?) RETURNING id")
            .bind(a.feature_id).bind(a.command).bind(a.engine_id)
            .bind(a.model).bind(a.provider_ref)
            .fetch_one(&self.pool).await?;
        Ok(id)
    }

    pub async fn recent(&self, limit: i64) -> Result<Vec<ActionRow>, KeaError> {
        let rows = sqlx::query_as::<_, (i64, String, String, String, String)>(
            "SELECT id, feature_id, command, engine_id, status
             FROM actions ORDER BY id DESC LIMIT ?")
            .bind(limit).fetch_all(&self.pool).await?;
        Ok(rows.into_iter().map(|(id, feature_id, command, engine_id, status)|
            ActionRow { id, feature_id, command, engine_id, status }).collect())
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-core store::actions`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core
git commit -m "feat(core): data.db + actions repository"
```

---

### Task 5: `CredentialStore` (keyring + in-memory)

**Files:**
- Create: `crates/core/src/secrets.rs`
- Modify: `crates/core/src/lib.rs` (`pub mod secrets;`)
- Test: `crates/core/src/secrets.rs` (inline, against `InMemoryCredentialStore`)

**Interfaces:**
- Produces: `#[async_trait] pub trait CredentialStore { async fn get(&self, provider_ref: &str) -> Result<Option<String>, KeaError>; async fn set(&self, provider_ref: &str, secret: &str) -> Result<(), KeaError>; async fn delete(&self, provider_ref: &str) -> Result<(), KeaError>; }`; `InMemoryCredentialStore` (tests/dev), `KeyringCredentialStore` (real).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_roundtrips() {
        let store = InMemoryCredentialStore::default();
        assert_eq!(store.get("openai").await.unwrap(), None);
        store.set("openai", "sk-test").await.unwrap();
        assert_eq!(store.get("openai").await.unwrap(), Some("sk-test".into()));
        store.delete("openai").await.unwrap();
        assert_eq!(store.get("openai").await.unwrap(), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-core secrets`
Expected: FAIL — types not found.

- [ ] **Step 3: Write the implementation**

```rust
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use crate::error::KeaError;

#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get(&self, provider_ref: &str) -> Result<Option<String>, KeaError>;
    async fn set(&self, provider_ref: &str, secret: &str) -> Result<(), KeaError>;
    async fn delete(&self, provider_ref: &str) -> Result<(), KeaError>;
}

#[derive(Default)]
pub struct InMemoryCredentialStore { map: Mutex<HashMap<String, String>> }

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn get(&self, p: &str) -> Result<Option<String>, KeaError> {
        Ok(self.map.lock().unwrap().get(p).cloned())
    }
    async fn set(&self, p: &str, s: &str) -> Result<(), KeaError> {
        self.map.lock().unwrap().insert(p.into(), s.into()); Ok(())
    }
    async fn delete(&self, p: &str) -> Result<(), KeaError> {
        self.map.lock().unwrap().remove(p); Ok(())
    }
}

pub struct KeyringCredentialStore { service: String }

impl KeyringCredentialStore {
    pub fn new(service: impl Into<String>) -> Self { Self { service: service.into() } }
    fn entry(&self, p: &str) -> Result<keyring::Entry, KeaError> {
        keyring::Entry::new(&self.service, p).map_err(|e| KeaError::Other(e.to_string()))
    }
}

#[async_trait]
impl CredentialStore for KeyringCredentialStore {
    async fn get(&self, p: &str) -> Result<Option<String>, KeaError> {
        match self.entry(p)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(KeaError::Other(e.to_string())),
        }
    }
    async fn set(&self, p: &str, s: &str) -> Result<(), KeaError> {
        self.entry(p)?.set_password(s).map_err(|e| KeaError::Other(e.to_string()))
    }
    async fn delete(&self, p: &str) -> Result<(), KeaError> {
        match self.entry(p)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(KeaError::Other(e.to_string())),
        }
    }
}
```

> The `KeyringCredentialStore` is **not** unit-tested in CI (headless runners lack a keyring backend). It is exercised manually on each OS during Phase 0 acceptance. Tests target `InMemoryCredentialStore`, which is also the default in dev builds.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-core secrets`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/secrets.rs crates/core/src/lib.rs
git commit -m "feat(core): CredentialStore trait with keyring + in-memory impls"
```

---

### Task 6: Logging (`tracing`) initialization

**Files:**
- Create: `crates/core/src/log.rs`
- Modify: `crates/core/src/lib.rs` (`pub mod log;`)
- Test: `crates/core/src/log.rs` (inline, writes to a tempdir)

**Interfaces:**
- Produces: `pub fn init_logging(log_dir: &std::path::Path, level: &str) -> tracing_appender::non_blocking::WorkerGuard`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tracing::info;

    #[test]
    fn writes_a_log_file() {
        let dir = tempfile::tempdir().unwrap();
        {
            let _guard = init_logging(dir.path(), "info");
            info!("hello-kea");
        } // guard drop flushes
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| e.ok()).collect();
        assert!(!files.is_empty(), "expected a log file to be created");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-core log::`
Expected: FAIL — `init_logging` not found.

- [ ] **Step 3: Write the implementation**

```rust
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

pub fn init_logging(log_dir: &Path, level: &str) -> WorkerGuard {
    let file_appender = tracing_appender::rolling::daily(log_dir, "kea.log");
    let (nb, guard) = tracing_appender::non_blocking(file_appender);
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(nb).with_ansi(false))
        .try_init();
    guard
}
```

> `try_init` (not `init`) so repeated calls in the test binary don't panic.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-core log::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/log.rs crates/core/src/lib.rs
git commit -m "feat(core): tracing logging init with rotating file appender"
```

---

### Task 7: Engine traits + `EngineRegistry`

**Files:**
- Create: `crates/engines/src/traits.rs`, `crates/engines/src/registry.rs`, `crates/engines/src/noop.rs`
- Replace: `crates/engines/src/lib.rs`
- Test: `crates/engines/src/registry.rs` (inline)

**Interfaces:**
- Produces: `LlmRequest { prompt: String }`, `LlmResponse { text: String }`, `EngineCaps { models: Vec<String> }`; `#[async_trait] trait LlmEngine: Send + Sync { fn id(&self) -> &str; fn capabilities(&self) -> EngineCaps; async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError>; }` (plus `SttEngine`, `TtsEngine` trait stubs); `EngineRegistry` with `register_llm(Arc<dyn LlmEngine>)`, `llm(&self, id) -> Option<Arc<dyn LlmEngine>>`, `list_llm_ids()`.
- Consumed by: `kea-core::resolve` (Task 9), `kea-features::demo` (Task 10).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::noop::NoopLlmEngine;
    use std::sync::Arc;

    #[test]
    fn register_and_lookup_llm() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));
        assert_eq!(reg.list_llm_ids(), vec!["noop".to_string()]);
        assert!(reg.llm("noop").is_some());
        assert!(reg.llm("missing").is_none());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-engines registry`
Expected: FAIL — types not found.

- [ ] **Step 3: Write traits, registry, noop, lib**

`crates/engines/src/traits.rs`:

```rust
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("engine error: {0}")]
    Other(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineCaps { pub models: Vec<String> }

#[derive(Debug, Clone, Deserialize)]
pub struct LlmRequest { pub prompt: String }

#[derive(Debug, Clone, Serialize)]
pub struct LlmResponse { pub text: String }

#[async_trait]
pub trait LlmEngine: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EngineCaps;
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError>;
}

// Stubs defined now so later phases implement against a stable shape.
#[async_trait]
pub trait SttEngine: Send + Sync { fn id(&self) -> &str; fn capabilities(&self) -> EngineCaps; }
#[async_trait]
pub trait TtsEngine: Send + Sync { fn id(&self) -> &str; fn capabilities(&self) -> EngineCaps; }
```

`crates/engines/src/registry.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use crate::traits::LlmEngine;

#[derive(Default)]
pub struct EngineRegistry {
    llm: HashMap<String, Arc<dyn LlmEngine>>,
}

impl EngineRegistry {
    pub fn register_llm(&mut self, e: Arc<dyn LlmEngine>) {
        self.llm.insert(e.id().to_string(), e);
    }
    pub fn llm(&self, id: &str) -> Option<Arc<dyn LlmEngine>> {
        self.llm.get(id).cloned()
    }
    pub fn list_llm_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.llm.keys().cloned().collect();
        v.sort();
        v
    }
}
```

`crates/engines/src/noop.rs`:

```rust
use async_trait::async_trait;
use crate::traits::{EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse};

pub struct NoopLlmEngine;

#[async_trait]
impl LlmEngine for NoopLlmEngine {
    fn id(&self) -> &str { "noop" }
    fn capabilities(&self) -> EngineCaps { EngineCaps { models: vec!["echo".into()] } }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
        Ok(LlmResponse { text: format!("echo: {}", req.prompt) })
    }
}
```

`crates/engines/src/lib.rs`:

```rust
pub mod traits;
pub mod registry;
pub mod noop;

pub use registry::EngineRegistry;
pub use traits::*;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-engines registry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/engines
git commit -m "feat(engines): engine traits + EngineRegistry + noop demo engine"
```

---

### Task 8: `Feature` trait + `FeatureRegistry`

**Files:**
- Create: `crates/features/src/feature.rs`, `crates/features/src/registry.rs`
- Replace: `crates/features/src/lib.rs`
- Test: `crates/features/src/registry.rs` (inline)

**Interfaces:**
- Produces: `CapSlot { name: &'static str, kind: CapKind }`, `enum CapKind { Llm, Stt, Tts }`; `trait Feature: Send + Sync { fn id(&self) -> &str; fn required_caps(&self) -> Vec<CapSlot>; }`; `FeatureRegistry` with `register(Arc<dyn Feature>)`, `get(id)`, `list_ids()`.
- Consumed by: `kea-features::demo` (Task 10), `src-tauri` (Task 11).

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::{CapKind, CapSlot, Feature};
    use std::sync::Arc;

    struct Fake;
    impl Feature for Fake {
        fn id(&self) -> &str { "fake" }
        fn required_caps(&self) -> Vec<CapSlot> {
            vec![CapSlot { name: "llm", kind: CapKind::Llm }]
        }
    }

    #[test]
    fn register_and_list() {
        let mut reg = FeatureRegistry::default();
        reg.register(Arc::new(Fake));
        assert_eq!(reg.list_ids(), vec!["fake".to_string()]);
        assert_eq!(reg.get("fake").unwrap().required_caps().len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-features registry`
Expected: FAIL — types not found.

- [ ] **Step 3: Write the implementation**

`crates/features/src/feature.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub enum CapKind { Llm, Stt, Tts }

#[derive(Debug, Clone, serde::Serialize)]
pub struct CapSlot { pub name: &'static str, pub kind: CapKind }

pub trait Feature: Send + Sync {
    fn id(&self) -> &str;
    fn required_caps(&self) -> Vec<CapSlot>;
}
```

`crates/features/src/registry.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use crate::feature::Feature;

#[derive(Default)]
pub struct FeatureRegistry { features: HashMap<String, Arc<dyn Feature>> }

impl FeatureRegistry {
    pub fn register(&mut self, f: Arc<dyn Feature>) {
        self.features.insert(f.id().to_string(), f);
    }
    pub fn get(&self, id: &str) -> Option<Arc<dyn Feature>> { self.features.get(id).cloned() }
    pub fn list_ids(&self) -> Vec<String> {
        let mut v: Vec<_> = self.features.keys().cloned().collect();
        v.sort(); v
    }
}
```

`crates/features/src/lib.rs`:

```rust
pub mod feature;
pub mod registry;
pub use feature::{CapKind, CapSlot, Feature};
pub use registry::FeatureRegistry;
```

Add `kea-engines = { path = "../engines" }` to `crates/features/Cargo.toml` (needed in Task 10).

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-features registry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/features
git commit -m "feat(features): Feature trait + FeatureRegistry + capability slots"
```

---

### Task 9: Slot resolution + binding repository

**Files:**
- Create: `crates/core/src/store/bindings.rs` (replace placeholder), `crates/core/src/resolve.rs`
- Modify: `crates/core/src/lib.rs` (`pub mod resolve;`)
- Test: `crates/core/src/resolve.rs` (inline)

**Interfaces:**
- Consumes: `EngineRegistry` (engines), `SettingsRepo`/`config.db` pool, `BindingRepo`.
- Produces: `BindingRepo` (`get(feature_id, slot) -> Option<Binding>`, `set(feature_id, slot, Binding)`); `Binding { engine_id, model, provider_ref }`; `enum Resolution { Bound(String), NeedsChoice(Vec<String>), Unresolvable }`; `SlotResolver::resolve_llm(&self, feature_id, slot) -> Result<Resolution, KeaError>` implementing the three rules from the spec.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::db::{open_pool, run_config_migrations};
    use crate::store::bindings::{Binding, BindingRepo};
    use kea_engines::{EngineRegistry, noop::NoopLlmEngine};
    use std::sync::Arc;

    async fn setup() -> (BindingRepo, EngineRegistry) {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        (BindingRepo::new(pool), EngineRegistry::default())
    }

    #[tokio::test]
    async fn unbound_single_engine_autobinds() {
        let (bindings, mut reg) = setup().await;
        reg.register_llm(Arc::new(NoopLlmEngine));
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::Bound(id) if id == "noop"));
    }

    #[tokio::test]
    async fn unbound_multiple_engines_needs_choice() {
        let (bindings, mut reg) = setup().await;
        reg.register_llm(Arc::new(NoopLlmEngine));
        reg.register_llm(Arc::new(crate::resolve::tests::SecondNoop));
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::NeedsChoice(v) if v.len() == 2));
    }

    #[tokio::test]
    async fn bound_to_missing_engine_is_unresolvable() {
        let (bindings, reg) = setup().await;
        bindings.set("demo", "llm", Binding {
            engine_id: "ghost".into(), model: None, provider_ref: None }).await.unwrap();
        let r = SlotResolver::new(&reg, &bindings).resolve_llm("demo", "llm").await.unwrap();
        assert!(matches!(r, Resolution::Unresolvable));
    }

    // a second engine id for the multi-engine test
    use async_trait::async_trait;
    use kea_engines::{EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse};
    pub struct SecondNoop;
    #[async_trait]
    impl LlmEngine for SecondNoop {
        fn id(&self) -> &str { "noop2" }
        fn capabilities(&self) -> EngineCaps { EngineCaps { models: vec![] } }
        async fn complete(&self, _r: LlmRequest) -> Result<LlmResponse, EngineError> {
            Ok(LlmResponse { text: String::new() })
        }
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-core resolve`
Expected: FAIL — `BindingRepo` / `SlotResolver` / `Resolution` not found.

- [ ] **Step 3: Write binding repo + resolver**

`crates/core/src/store/bindings.rs`:

```rust
use sqlx::SqlitePool;
use crate::error::KeaError;

#[derive(Debug, Clone)]
pub struct Binding {
    pub engine_id: String,
    pub model: Option<String>,
    pub provider_ref: Option<String>,
}

pub struct BindingRepo { pool: SqlitePool }

impl BindingRepo {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }

    pub async fn get(&self, feature_id: &str, slot: &str) -> Result<Option<Binding>, KeaError> {
        let row: Option<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT engine_id, model, provider_ref FROM bindings
             WHERE feature_id = ? AND slot = ?")
            .bind(feature_id).bind(slot).fetch_optional(&self.pool).await?;
        Ok(row.map(|(engine_id, model, provider_ref)| Binding { engine_id, model, provider_ref }))
    }

    pub async fn set(&self, feature_id: &str, slot: &str, b: Binding) -> Result<(), KeaError> {
        sqlx::query(
            "INSERT INTO bindings(feature_id, slot, engine_id, model, provider_ref)
             VALUES(?, ?, ?, ?, ?)
             ON CONFLICT(feature_id, slot) DO UPDATE SET
               engine_id = excluded.engine_id, model = excluded.model,
               provider_ref = excluded.provider_ref")
            .bind(feature_id).bind(slot).bind(b.engine_id).bind(b.model).bind(b.provider_ref)
            .execute(&self.pool).await?;
        Ok(())
    }
}
```

`crates/core/src/resolve.rs`:

```rust
use kea_engines::EngineRegistry;
use crate::error::KeaError;
use crate::store::bindings::BindingRepo;

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Bound(String),            // engine_id chosen
    NeedsChoice(Vec<String>), // candidate engine ids
    Unresolvable,             // bound to a missing engine, or no candidates
}

pub struct SlotResolver<'a> {
    engines: &'a EngineRegistry,
    bindings: &'a BindingRepo,
}

impl<'a> SlotResolver<'a> {
    pub fn new(engines: &'a EngineRegistry, bindings: &'a BindingRepo) -> Self {
        Self { engines, bindings }
    }

    pub async fn resolve_llm(&self, feature_id: &str, slot: &str) -> Result<Resolution, KeaError> {
        if let Some(b) = self.bindings.get(feature_id, slot).await? {
            return Ok(if self.engines.llm(&b.engine_id).is_some() {
                Resolution::Bound(b.engine_id)
            } else {
                Resolution::Unresolvable
            });
        }
        let candidates = self.engines.list_llm_ids();
        Ok(match candidates.len() {
            0 => Resolution::Unresolvable,
            1 => Resolution::Bound(candidates.into_iter().next().unwrap()),
            _ => Resolution::NeedsChoice(candidates),
        })
    }
}

#[cfg(test)]
pub mod tests; // test module lives in this file via the block above; see Step 1
```

> Adjust the `#[cfg(test)] pub mod tests;` to an inline `#[cfg(test)] mod tests { ... }` block containing the Step-1 code (the `pub struct SecondNoop` is referenced as `crate::resolve::tests::SecondNoop`). Add `kea-engines` to `crates/core/Cargo.toml` `[dev-dependencies]` and `[dependencies]`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-core resolve`
Expected: PASS (all three rules).

- [ ] **Step 5: Commit**

```bash
git add crates/core
git commit -m "feat(core): slot resolution + binding repository (auto-bind/choice/unresolvable)"
```

---

### Task 10: Demo feature end-to-end (registration → resolution → run)

**Files:**
- Create: `crates/features/src/demo.rs`
- Modify: `crates/features/src/lib.rs` (`pub mod demo;`)
- Test: `crates/features/src/demo.rs` (inline integration-style test)

**Interfaces:**
- Consumes: `Feature`, `EngineRegistry`, `NoopLlmEngine`.
- Produces: `DemoFeature`; `async fn run_ping(engines: &EngineRegistry, engine_id: &str, prompt: &str) -> Result<String, String>` — the function `src-tauri`'s `run_demo` command calls.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kea_engines::{EngineRegistry, noop::NoopLlmEngine};
    use crate::feature::Feature;
    use std::sync::Arc;

    #[test]
    fn declares_one_llm_slot() {
        let f = DemoFeature;
        assert_eq!(f.id(), "demo");
        assert_eq!(f.required_caps().len(), 1);
    }

    #[tokio::test]
    async fn run_ping_routes_through_resolved_engine() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));
        let out = run_ping(&reg, "noop", "hi").await.unwrap();
        assert_eq!(out, "echo: hi");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-features demo`
Expected: FAIL — `DemoFeature` / `run_ping` not found.

- [ ] **Step 3: Write the implementation**

```rust
use kea_engines::{EngineRegistry, LlmRequest};
use crate::feature::{CapKind, CapSlot, Feature};

pub struct DemoFeature;

impl Feature for DemoFeature {
    fn id(&self) -> &str { "demo" }
    fn required_caps(&self) -> Vec<CapSlot> {
        vec![CapSlot { name: "llm", kind: CapKind::Llm }]
    }
}

pub async fn run_ping(engines: &EngineRegistry, engine_id: &str, prompt: &str)
    -> Result<String, String>
{
    let engine = engines.llm(engine_id)
        .ok_or_else(|| format!("no llm engine '{engine_id}'"))?;
    let resp = engine.complete(LlmRequest { prompt: prompt.to_string() })
        .await.map_err(|e| e.to_string())?;
    Ok(resp.text)
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p kea-features demo`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/features
git commit -m "feat(features): demo feature exercising resolution -> engine run"
```

---

### Task 11: `src-tauri` — app state, migrations, tray, commands

**Files:**
- Create: `src-tauri/src/commands.rs`
- Replace: `src-tauri/src/main.rs`
- Test: `src-tauri/src/commands.rs` (inline — test the plain functions the commands delegate to)

**Interfaces:**
- Consumes: everything above.
- Produces: Tauri commands `list_engines`, `list_features`, `get_setting`, `set_setting`, `get_binding`, `set_binding`, `run_demo`; an `AppState { engines, features, config_pool, data_pool }` built in `setup`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kea_engines::{EngineRegistry, noop::NoopLlmEngine};
    use std::sync::Arc;

    #[test]
    fn engine_ids_listing_is_pure() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(NoopLlmEngine));
        assert_eq!(engine_ids(&reg), vec!["noop".to_string()]);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p kea-app commands`
Expected: FAIL — `engine_ids` not found.

- [ ] **Step 3: Write commands + main**

`src-tauri/src/commands.rs`:

```rust
use kea_engines::EngineRegistry;

/// Pure helper kept separate from the #[tauri::command] wrapper so it is unit-testable.
pub fn engine_ids(reg: &EngineRegistry) -> Vec<String> { reg.list_llm_ids() }
```

`src-tauri/src/main.rs`:

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;

use std::sync::Arc;
use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
use kea_core::store::settings::SettingsRepo;
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::resolve::{Resolution, SlotResolver};
use kea_engines::{EngineRegistry, noop::NoopLlmEngine};
use kea_features::{FeatureRegistry, demo::{DemoFeature, run_ping}};
use sqlx::SqlitePool;
use tauri::{Manager, State};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;

struct AppState {
    engines: EngineRegistry,
    features: FeatureRegistry,
    config_pool: SqlitePool,
}

#[tauri::command]
fn list_engines(state: State<AppState>) -> Vec<String> { commands::engine_ids(&state.engines) }

#[tauri::command]
fn list_features(state: State<AppState>) -> Vec<String> { state.features.list_ids() }

#[tauri::command]
async fn get_setting(state: State<'_, AppState>, key: String) -> Result<Option<String>, String> {
    SettingsRepo::new(state.config_pool.clone()).get(&key).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_setting(state: State<'_, AppState>, key: String, value: String) -> Result<(), String> {
    SettingsRepo::new(state.config_pool.clone()).set(&key, &value).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn set_binding(state: State<'_, AppState>, feature: String, slot: String, engine: String)
    -> Result<(), String> {
    BindingRepo::new(state.config_pool.clone())
        .set(&feature, &slot, Binding { engine_id: engine, model: None, provider_ref: None })
        .await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn run_demo(state: State<'_, AppState>, prompt: String) -> Result<String, String> {
    let bindings = BindingRepo::new(state.config_pool.clone());
    let resolver = SlotResolver::new(&state.engines, &bindings);
    let engine_id = match resolver.resolve_llm("demo", "llm").await.map_err(|e| e.to_string())? {
        Resolution::Bound(id) => id,
        Resolution::NeedsChoice(v) => return Err(format!("choose an engine: {v:?}")),
        Resolution::Unresolvable => return Err("no engine bound".into()),
    };
    run_ping(&state.engines, &engine_id, &prompt).await
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir().expect("app data dir");
            std::fs::create_dir_all(&dir).ok();
            let log_dir = app.path().app_log_dir().unwrap_or(dir.clone());
            std::fs::create_dir_all(&log_dir).ok();
            let _guard = kea_core::log::init_logging(&log_dir, "info");
            app.manage_guard(_guard);

            let config_url = format!("sqlite://{}?mode=rwc", dir.join("config.db").display());
            let data_url = format!("sqlite://{}?mode=rwc", dir.join("data.db").display());

            let rt = tokio::runtime::Handle::current();
            let (config_pool, _data_pool) = tauri::async_runtime::block_on(async {
                let c = open_pool(&config_url).await.unwrap();
                run_config_migrations(&c).await.unwrap();
                let d = open_pool(&data_url).await.unwrap();
                run_data_migrations(&d).await.unwrap();
                (c, d)
            });
            let _ = rt;

            let mut engines = EngineRegistry::default();
            engines.register_llm(Arc::new(NoopLlmEngine));
            let mut features = FeatureRegistry::default();
            features.register(Arc::new(DemoFeature));

            app.manage(AppState { engines, features, config_pool });

            let quit = MenuItem::with_id(app, "quit", "Quit KEA", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quit])?;
            TrayIconBuilder::new()
                .menu(&menu)
                .on_menu_event(|app, e| if e.id() == "quit" { app.exit(0) })
                .build(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_engines, list_features, get_setting, set_setting, set_binding, run_demo
        ])
        .run(tauri::generate_context!())
        .expect("error while running KEA");
}
```

> `app.manage_guard` is shorthand: store the `WorkerGuard` in managed state (`app.manage(_guard)`) so it lives for the app's lifetime — replace the line with `app.manage(_guard);` and drop the helper name. `get_binding` is exposed by adding a `#[tauri::command]` mirroring `set_binding` using `BindingRepo::get`; include it in `generate_handler!`.

- [ ] **Step 4: Run test + build the app crate**

Run: `cargo test -p kea-app commands && cargo build -p kea-app`
Expected: PASS + the app crate compiles. (Full `tauri dev` is exercised manually in acceptance, since it needs the UI from Task 12.)

- [ ] **Step 5: Commit**

```bash
git add src-tauri
git commit -m "feat(app): tauri state, startup migrations, tray, plugin commands"
```

---

### Task 12: React UI shell + typed API

**Files:**
- Create: `ui/package.json`, `ui/vite.config.ts`, `ui/index.html`, `ui/src/main.tsx`, `ui/src/api.ts`, `ui/src/App.tsx`, `ui/tsconfig.json`
- Test: `ui/src/api.ts` is type-checked via `tsc --noEmit` (build gate); no runtime unit test framework in Phase 0.

**Interfaces:**
- Consumes: Tauri commands from Task 11 via `@tauri-apps/api`.
- Produces: a window showing engines/features and a demo "ping" box; `npm run build` produces `ui/dist` that `tauri.conf.json` points at.

- [ ] **Step 1: Write the failing check**

Create `ui/package.json`:

```json
{
  "name": "kea-ui",
  "private": true,
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "tsc --noEmit && vite build",
    "typecheck": "tsc --noEmit"
  },
  "dependencies": { "react": "^18", "react-dom": "^18", "@tauri-apps/api": "^2" },
  "devDependencies": {
    "@types/react": "^18", "@types/react-dom": "^18",
    "typescript": "^5", "vite": "^5", "@vitejs/plugin-react": "^4"
  }
}
```

Run: `cd ui && npm install && npm run typecheck`
Expected: FAIL — no source files / config yet.

- [ ] **Step 2: Add config + source**

`ui/vite.config.ts`:

```ts
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
export default defineConfig({ plugins: [react()], clearScreen: false, server: { port: 5173, strictPort: true } });
```

`ui/tsconfig.json`:

```json
{
  "compilerOptions": {
    "target": "ES2020", "module": "ESNext", "moduleResolution": "Bundler",
    "jsx": "react-jsx", "strict": true, "skipLibCheck": true, "noEmit": true
  },
  "include": ["src"]
}
```

`ui/index.html`:

```html
<!doctype html>
<html><head><meta charset="utf-8" /><title>KEA</title></head>
<body><div id="root"></div><script type="module" src="/src/main.tsx"></script></body></html>
```

`ui/src/api.ts`:

```ts
import { invoke } from "@tauri-apps/api/core";

export const listEngines = () => invoke<string[]>("list_engines");
export const listFeatures = () => invoke<string[]>("list_features");
export const getSetting = (key: string) => invoke<string | null>("get_setting", { key });
export const setSetting = (key: string, value: string) => invoke<void>("set_setting", { key, value });
export const setBinding = (feature: string, slot: string, engine: string) =>
  invoke<void>("set_binding", { feature, slot, engine });
export const runDemo = (prompt: string) => invoke<string>("run_demo", { prompt });
```

`ui/src/App.tsx`:

```tsx
import { useEffect, useState } from "react";
import { listEngines, listFeatures, runDemo } from "./api";

export default function App() {
  const [engines, setEngines] = useState<string[]>([]);
  const [features, setFeatures] = useState<string[]>([]);
  const [prompt, setPrompt] = useState("hello");
  const [out, setOut] = useState("");

  useEffect(() => { listEngines().then(setEngines); listFeatures().then(setFeatures); }, []);

  return (
    <main style={{ fontFamily: "system-ui", padding: 24 }}>
      <h1>KEA</h1>
      <p>Engines: {engines.join(", ") || "—"}</p>
      <p>Features: {features.join(", ") || "—"}</p>
      <input value={prompt} onChange={e => setPrompt(e.target.value)} />
      <button onClick={async () => setOut(await runDemo(prompt))}>Run demo</button>
      <pre>{out}</pre>
    </main>
  );
}
```

`ui/src/main.tsx`:

```tsx
import React from "react";
import { createRoot } from "react-dom/client";
import App from "./App";
createRoot(document.getElementById("root")!).render(<React.StrictMode><App /></React.StrictMode>);
```

- [ ] **Step 3: Run the check + build**

Run: `cd ui && npm run build`
Expected: PASS — `tsc --noEmit` clean, `ui/dist` produced.

- [ ] **Step 4: Manual end-to-end smoke (acceptance)**

Run: `cargo tauri dev` (from repo root, with the Tauri CLI installed).
Expected: window opens showing `Engines: noop`, `Features: demo`; typing `hi` + "Run demo" shows `echo: hi`; tray has "Quit KEA"; `config.db`, `data.db`, and `kea.log` appear in the app data/log dirs.

- [ ] **Step 5: Commit**

```bash
git add ui
git commit -m "feat(ui): React shell with typed Tauri API + demo runner"
```

---

### Task 13: CI matrix (macOS / Windows / Linux)

**Files:**
- Create: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: CI that runs `cargo test --workspace`, `cargo build -p kea-app`, and `ui` build on all three OSes.

- [ ] **Step 1: Write the workflow**

`.github/workflows/ci.yml`:

```yaml
name: CI
on: [push, pull_request]
jobs:
  build-test:
    strategy:
      fail-fast: false
      matrix:
        os: [ubuntu-latest, windows-latest, macos-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2
      - name: Linux Tauri/webkit deps
        if: matrix.os == 'ubuntu-latest'
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libasound2-dev
      - uses: actions/setup-node@v4
        with: { node-version: 20 }
      - name: Build UI
        run: cd ui && npm install && npm run build
      - name: Rust tests
        run: cargo test --workspace
      - name: Build app crate
        run: cargo build -p kea-app
```

- [ ] **Step 2: Verify locally what CI runs**

Run: `cargo test --workspace && cargo build -p kea-app && (cd ui && npm install && npm run build)`
Expected: all PASS locally before relying on CI.

- [ ] **Step 3: Commit + push**

```bash
git add .github/workflows/ci.yml
git commit -m "ci: build + test workspace and ui on macOS/Windows/Linux"
git push
```

- [ ] **Step 4: Confirm green**

Open the Actions run for the pushed branch; confirm all three OS jobs pass.

---

## Phase 0 Definition of Done

- `cargo test --workspace` green; `cargo build -p kea-app` succeeds; `ui` builds — all three on CI.
- `cargo tauri dev` launches a window on macOS/Windows/Linux showing `Engines: noop`, `Features: demo`, and a working demo runner returning `echo: <prompt>`.
- `config.db` and `data.db` are created and migrated on first launch; `kea.log` is written.
- Tray with "Quit KEA"; app identity is KEA (`ai.kea.app`).
- Slot resolution covers all three rules (auto-bind / needs-choice / unresolvable), tested.
- `CredentialStore` abstraction in place (keyring + in-memory), in-memory tested.

## Self-Review Notes (coverage against spec §-by-§)

- §3 D1/D2 (registries) → Tasks 7, 8. D9 (config.db/data.db, keyring, CredentialStore) → Tasks 2–5. D10 (React) → Task 12. D13 (KEA name) → Task 1.
- §4.1/4.2/4.3 (engine/platform/feature traits) → Tasks 7, 8 (platform trait stubs are scaffolded in Task 1's `platform/src/lib.rs`; real impls are Phase 1+).
- §4.4 slot resolution (three rules) → Task 9.
- §6.1/6.3 (config + data DBs, migrations) → Tasks 2–4; §6.4 (logging) → Task 6.
- §7 (React shell + typed command contracts) → Tasks 11–12 (full component library is Phase 1+).
- §9 Phase 0 outcomes → Definition of Done above.
- Deferred to later phases by design: real engines (Whisper/OpenAI/Parakeet/TTS), platform impls (hotkeys/textio/audio), `infer/` plumbing, History/Logs UI pages, retention controls.
