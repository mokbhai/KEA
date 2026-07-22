# Cross-Platform Rewrite — Phase 0 (Foundation) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up a Tauri 2.x application that launches on macOS, Windows, and Linux, persists user settings to disk, stores secrets in the OS keychain, shows a tray icon and a settings window, and builds runnable artifacts for all three OSes in CI.

**Architecture:** A Cargo workspace with platform-agnostic Rust crates (`core` for settings + secrets) plus the Tauri shell (`src-tauri`) that exposes Rust functionality to a React/Vite/TypeScript web UI (`ui`) via Tauri commands. This phase deliberately contains no rewrite/speech logic — it proves the cross-platform skeleton, the settings round-trip, and the CI matrix.

**Tech Stack:** Rust (edition 2021), Tauri 2.x, `serde`/`serde_json`, `keyring` crate, `directories` crate, React 18 + TypeScript + Vite, GitHub Actions (macos-latest, windows-latest, ubuntu-latest).

**Reference spec:** `docs/cross-platform/2026-05-29-cross-platform-rewrite-design.md`

---

## File Structure

The new app lives in a top-level `app/` directory so it coexists with the
legacy Swift project until parity is reached (per Decision D4).

- `app/Cargo.toml` — Cargo workspace manifest (members: `core`, `src-tauri`).
- `app/crates/core/Cargo.toml` — the platform-agnostic core library crate.
- `app/crates/core/src/lib.rs` — re-exports `settings` and `secrets` modules.
- `app/crates/core/src/settings.rs` — settings schema + load/save store.
- `app/crates/core/src/secrets.rs` — keyring-backed secret storage + trait.
- `app/src-tauri/Cargo.toml` — Tauri binary crate; depends on `core`.
- `app/src-tauri/tauri.conf.json` — Tauri config (windows, tray, identifier).
- `app/src-tauri/src/main.rs` — Tauri entrypoint, command registration, tray.
- `app/src-tauri/src/commands.rs` — Tauri commands wrapping `core`.
- `app/ui/package.json`, `app/ui/vite.config.ts`, `app/ui/index.html` — web UI.
- `app/ui/src/main.tsx`, `app/ui/src/App.tsx` — minimal settings page.
- `.github/workflows/app-ci.yml` — 3-OS build matrix for the new app.

Each file has one responsibility: `settings.rs` knows only the schema and disk
persistence; `secrets.rs` knows only keychain access; `commands.rs` only adapts
core types to Tauri; `main.rs` only wires the shell and tray.

---

## Prerequisites (one-time, not committed)

- [ ] **Step 0: Verify the Rust + Tauri toolchain is installed**

Run:
```bash
rustc --version && cargo --version
cargo install create-tauri-app --locked 2>/dev/null || true
node --version && npm --version
```
Expected: `rustc`/`cargo` print versions (install via `https://rustup.rs` if
missing); Node ≥ 18 prints a version. On Linux also install Tauri's system deps:
```bash
sudo apt-get update && sudo apt-get install -y libwebkit2gtk-4.1-dev \
  build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
```

---

## Task 1: Scaffold the Cargo workspace + Tauri app

**Files:**
- Create: `app/Cargo.toml`
- Create: `app/crates/core/Cargo.toml`, `app/crates/core/src/lib.rs`
- Create: `app/src-tauri/Cargo.toml`, `app/src-tauri/src/main.rs`, `app/src-tauri/tauri.conf.json`, `app/src-tauri/build.rs`
- Create: `app/ui/package.json`, `app/ui/vite.config.ts`, `app/ui/index.html`, `app/ui/src/main.tsx`, `app/ui/src/App.tsx`, `app/ui/tsconfig.json`

- [ ] **Step 1: Generate the Tauri app skeleton**

Run:
```bash
mkdir -p app && cd app
npm create tauri-app@latest . -- --template react-ts --manager npm --yes
```
This creates `src-tauri/`, `package.json`, `vite.config.ts`, `index.html`,
`src/`. Move the web sources under `ui/` for clarity:
```bash
mkdir -p ui && git mv -k src ui/src 2>/dev/null || mv src ui/src
mv index.html vite.config.ts tsconfig.json package.json ui/ 2>/dev/null || true
```
Then update `ui/vite.config.ts` `root`/paths and `src-tauri/tauri.conf.json`
`build.frontendDist` to point at `../ui/dist` and `build.devUrl` to the Vite dev
server. (Exact keys shown in Step 3.)

- [ ] **Step 2: Convert to a Cargo workspace**

Create `app/Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["crates/core", "src-tauri"]

[workspace.package]
edition = "2021"
version = "0.0.0"
license = "Apache-2.0"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
```

Create `app/crates/core/Cargo.toml`:
```toml
[package]
name = "vox-core"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
serde = { workspace = true }
serde_json = { workspace = true }
keyring = "3"
directories = "5"
thiserror = "1"

[dev-dependencies]
tempfile = "3"
```

Create `app/crates/core/src/lib.rs`:
```rust
pub mod settings;
pub mod secrets;
```

Edit `app/src-tauri/Cargo.toml` to add the core dependency under `[dependencies]`:
```toml
vox-core = { path = "../crates/core" }
```

- [ ] **Step 3: Configure Tauri windows + identifier**

Edit `app/src-tauri/tauri.conf.json` so it contains (merge with generated keys):
```json
{
  "productName": "Vox",
  "identifier": "com.voxapp.rewrite",
  "build": {
    "frontendDist": "../ui/dist",
    "devUrl": "http://localhost:5173",
    "beforeDevCommand": "npm --prefix ../ui run dev",
    "beforeBuildCommand": "npm --prefix ../ui run build"
  },
  "app": {
    "windows": [
      { "label": "settings", "title": "Vox Settings", "width": 720, "height": 520, "visible": true }
    ]
  }
}
```

- [ ] **Step 4: Verify it builds and runs**

Run:
```bash
cd app && npm --prefix ui install && cargo build
```
Expected: workspace compiles; `cargo build` succeeds for `vox-core` and the
`vox` binary. (A full `npm --prefix ui run tauri dev` should open a window, but
that is a manual check — not required for the commit.)

- [ ] **Step 5: Commit**

```bash
git add app .gitignore
git commit -m "feat(app): scaffold Tauri workspace (core + src-tauri + ui)"
```
(Add `app/target/`, `app/ui/node_modules/`, `app/ui/dist/` to `.gitignore` before committing.)

---

## Task 2: Settings schema + serialization

**Files:**
- Modify: `app/crates/core/src/settings.rs`
- Test: in-file `#[cfg(test)]` module in `settings.rs`

- [ ] **Step 1: Write the failing test**

Add to `app/crates/core/src/settings.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    pub rewrite_hotkey: String,
    pub speech_hotkey: String,
    pub launch_at_login: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: 1,
            rewrite_hotkey: "CmdOrCtrl+Shift+R".to_string(),
            speech_hotkey: "CmdOrCtrl+Shift+S".to_string(),
            launch_at_login: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip_through_json() {
        let original = Settings::default();
        let json = serde_json::to_string(&original).unwrap();
        let parsed: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(original, parsed);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let parsed: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, Settings::default());
    }
}
```

- [ ] **Step 2: Run test to verify it fails (then passes)**

Run: `cd app && cargo test -p vox-core settings`
Expected: compiles and both tests PASS (this task is schema-only; the test
guards `#[serde(default)]` behavior and the `Default` impl).

- [ ] **Step 3: Commit**

```bash
git add app/crates/core/src/settings.rs
git commit -m "feat(core): add Settings schema with serde defaults"
```

---

## Task 3: Settings store (load/save to disk)

**Files:**
- Modify: `app/crates/core/src/settings.rs`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `settings.rs`:
```rust
    #[test]
    fn save_then_load_returns_same_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let store = SettingsStore::at(path.clone());

        let mut s = Settings::default();
        s.rewrite_hotkey = "CmdOrCtrl+Alt+R".to_string();
        store.save(&s).unwrap();

        let loaded = store.load().unwrap();
        assert_eq!(loaded, s);
    }

    #[test]
    fn load_missing_file_returns_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::at(dir.path().join("nope.json"));
        assert_eq!(store.load().unwrap(), Settings::default());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd app && cargo test -p vox-core settings`
Expected: FAIL — `SettingsStore` is not defined.

- [ ] **Step 3: Write minimal implementation**

Add to `settings.rs` (above the tests module):
```rust
use std::fs;
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialize error: {0}")]
    Serde(#[from] serde_json::Error),
}

pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn load(&self) -> Result<Settings, SettingsError> {
        match fs::read_to_string(&self.path) {
            Ok(text) => Ok(serde_json::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
            Err(e) => Err(e.into()),
        }
    }

    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(settings)?;
        fs::write(&self.path, text)?;
        Ok(())
    }
}

/// Resolve the default per-user settings path for this OS.
pub fn default_settings_path() -> PathBuf {
    let proj = directories::ProjectDirs::from("com", "voxapp", "Vox")
        .expect("a valid home directory is required");
    proj.config_dir().join("settings.json")
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd app && cargo test -p vox-core settings`
Expected: all four settings tests PASS.

- [ ] **Step 5: Commit**

```bash
git add app/crates/core/src/settings.rs
git commit -m "feat(core): add SettingsStore disk persistence"
```

---

## Task 4: Secrets storage (keychain-backed)

**Files:**
- Modify: `app/crates/core/src/secrets.rs`

- [ ] **Step 1: Write the failing test**

Create `app/crates/core/src/secrets.rs`:
```rust
/// Abstraction over per-account secret storage so callers (and tests) do not
/// depend on the OS keychain directly.
pub trait SecretStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError>;
    fn get(&self, account: &str) -> Result<Option<String>, SecretError>;
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keyring error: {0}")]
    Keyring(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryStore(Mutex<HashMap<String, String>>);

    impl SecretStore for MemoryStore {
        fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().insert(account.into(), secret.into());
            Ok(())
        }
        fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
            Ok(self.0.lock().unwrap().get(account).cloned())
        }
        fn delete(&self, account: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().remove(account);
            Ok(())
        }
    }

    #[test]
    fn set_get_delete_cycle() {
        let store = MemoryStore::default();
        assert_eq!(store.get("openai").unwrap(), None);
        store.set("openai", "sk-123").unwrap();
        assert_eq!(store.get("openai").unwrap(), Some("sk-123".to_string()));
        store.delete("openai").unwrap();
        assert_eq!(store.get("openai").unwrap(), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd app && cargo test -p vox-core secrets`
Expected: FAIL — `SecretStore`/`SecretError` not found until the file compiles;
the test exercises only the trait via an in-memory fake.

- [ ] **Step 3: Write the keyring-backed implementation**

Append to `secrets.rs`:
```rust
const SERVICE: &str = "com.voxapp.rewrite";

/// Production `SecretStore` backed by the OS keychain
/// (Keychain / Windows Credential Manager / libsecret).
pub struct KeyringStore;

impl SecretStore for KeyringStore {
    fn set(&self, account: &str, secret: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        entry.set_password(secret).map_err(|e| SecretError::Keyring(e.to_string()))
    }
    fn get(&self, account: &str) -> Result<Option<String>, SecretError> {
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        match entry.get_password() {
            Ok(p) => Ok(Some(p)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
    fn delete(&self, account: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(SERVICE, account)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd app && cargo test -p vox-core secrets`
Expected: `set_get_delete_cycle` PASSES. (The `KeyringStore` is exercised
manually in Task 5 / by integration, not in unit tests, to avoid touching the
real keychain in CI.)

- [ ] **Step 5: Commit**

```bash
git add app/crates/core/src/secrets.rs
git commit -m "feat(core): add SecretStore trait + keyring implementation"
```

---

## Task 5: Tauri commands wrapping core

**Files:**
- Create: `app/src-tauri/src/commands.rs`
- Modify: `app/src-tauri/src/main.rs`

- [ ] **Step 1: Write the commands module**

Create `app/src-tauri/src/commands.rs`:
```rust
use vox_core::secrets::{KeyringStore, SecretStore};
use vox_core::settings::{default_settings_path, Settings, SettingsStore};

fn store() -> SettingsStore {
    SettingsStore::at(default_settings_path())
}

#[tauri::command]
pub fn load_settings() -> Result<Settings, String> {
    store().load().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_settings(settings: Settings) -> Result<(), String> {
    store().save(&settings).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_secret(account: String, secret: String) -> Result<(), String> {
    KeyringStore.set(&account, &secret).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn has_secret(account: String) -> Result<bool, String> {
    KeyringStore.get(&account).map(|v| v.is_some()).map_err(|e| e.to_string())
}
```

- [ ] **Step 2: Register the commands**

Edit `app/src-tauri/src/main.rs` so `main` registers the handlers:
```rust
mod commands;

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::load_settings,
            commands::save_settings,
            commands::set_secret,
            commands::has_secret,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 3: Verify it compiles**

Run: `cd app && cargo build -p vox`
Expected: builds with no errors; the four commands are registered.

- [ ] **Step 4: Commit**

```bash
git add app/src-tauri/src/commands.rs app/src-tauri/src/main.rs
git commit -m "feat(app): expose settings + secrets via Tauri commands"
```

---

## Task 6: Tray icon + settings window

**Files:**
- Modify: `app/src-tauri/src/main.rs`
- Modify: `app/src-tauri/tauri.conf.json` (ensure a tray icon asset exists)

- [ ] **Step 1: Add the tray with a menu**

Edit `app/src-tauri/src/main.rs` to build a tray in `setup`:
```rust
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager,
};

mod commands;

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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::load_settings,
            commands::save_settings,
            commands::set_secret,
            commands::has_secret,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```
Ensure `Cargo.toml` for `vox` enables the tray feature:
```toml
tauri = { version = "2", features = ["tray-icon"] }
```

- [ ] **Step 2: Verify it compiles**

Run: `cd app && cargo build -p vox`
Expected: builds; tray code type-checks against Tauri 2 APIs.

- [ ] **Step 3: Manual smoke check (not committed gate)**

Run: `cd app && npm --prefix ui run tauri dev`
Expected: a tray icon appears; "Settings…" shows the window; "Quit" exits.

- [ ] **Step 4: Commit**

```bash
git add app/src-tauri/src/main.rs app/src-tauri/Cargo.toml app/src-tauri/tauri.conf.json
git commit -m "feat(app): add system tray with settings + quit"
```

---

## Task 7: Minimal settings UI (round-trips a setting)

**Files:**
- Modify: `app/ui/src/App.tsx`

- [ ] **Step 1: Replace App with a settings form**

Set `app/ui/src/App.tsx` to:
```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

type Settings = {
  schema_version: number;
  rewrite_hotkey: string;
  speech_hotkey: string;
  launch_at_login: boolean;
};

export default function App() {
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
        style={{ display: "block", marginTop: 12 }}
        onClick={async () => {
          await invoke("save_settings", { settings });
          setStatus("Saved");
        }}
      >
        Save
      </button>
      <p>{status}</p>
    </main>
  );
}
```

- [ ] **Step 2: Verify the UI builds**

Run: `cd app && npm --prefix ui run build`
Expected: Vite build succeeds, `ui/dist` is produced.

- [ ] **Step 3: Manual smoke check (not committed gate)**

Run: `cd app && npm --prefix ui run tauri dev`
Expected: window loads current hotkey; editing + Save persists; relaunch shows
the saved value (verifies the full UI → command → disk round-trip).

- [ ] **Step 4: Commit**

```bash
git add app/ui/src/App.tsx
git commit -m "feat(ui): minimal settings form round-tripping through core"
```

---

## Task 8: CI build matrix for all three OSes

**Files:**
- Create: `.github/workflows/app-ci.yml`

- [ ] **Step 1: Write the workflow**

Create `.github/workflows/app-ci.yml`:
```yaml
name: App CI

on:
  push:
    paths: ["app/**", ".github/workflows/app-ci.yml"]
  pull_request:
    paths: ["app/**", ".github/workflows/app-ci.yml"]

jobs:
  build:
    strategy:
      fail-fast: false
      matrix:
        os: [macos-latest, windows-latest, ubuntu-latest]
    runs-on: ${{ matrix.os }}
    timeout-minutes: 45
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@stable
      - uses: actions/setup-node@v4
        with:
          node-version: 20
      - name: Install Linux system deps
        if: matrix.os == 'ubuntu-latest'
        run: |
          sudo apt-get update
          sudo apt-get install -y libwebkit2gtk-4.1-dev build-essential curl wget file \
            libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev
      - name: Install UI deps
        run: npm --prefix app/ui ci || npm --prefix app/ui install
      - name: Rust unit tests
        run: cargo test --manifest-path app/Cargo.toml -p vox-core
      - name: Build app
        run: |
          npm --prefix app/ui run build
          cargo build --manifest-path app/Cargo.toml -p vox
```

- [ ] **Step 2: Verify locally where possible**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core && npm --prefix app/ui run build`
Expected: tests pass and the UI builds (mirrors the CI steps on the current OS).

- [ ] **Step 3: Commit and push to trigger CI**

```bash
git add .github/workflows/app-ci.yml
git commit -m "ci: build the cross-platform app on macOS, Windows, and Linux"
git push
```
Expected: the **App CI** workflow runs and succeeds on all three OS runners
(verify with `gh run list --workflow=app-ci.yml`).

---

## Task 9: Document the new app + retirement note

**Files:**
- Create: `app/README.md`
- Modify: `README.md` (add a pointer to the cross-platform app)

- [ ] **Step 1: Write `app/README.md`**

```markdown
# Vox (cross-platform)

The cross-platform rewrite of Vox (macOS, Windows, Linux) built on Tauri.
See `docs/cross-platform/2026-05-29-cross-platform-rewrite-design.md` for the
design and `docs/cross-platform/plans/` for phase plans.

## Develop
    npm --prefix ui install
    npm --prefix ui run tauri dev

## Test
    cargo test -p vox-core

## Build
    npm --prefix ui run build && cargo build -p vox
```

- [ ] **Step 2: Add a pointer in the root `README.md`**

Add under the title in `README.md`:
```markdown
> A cross-platform rewrite (macOS/Windows/Linux) is in progress under `app/`.
> See `docs/cross-platform/`. The Swift app remains the shipping macOS product
> until the rewrite reaches parity.
```

- [ ] **Step 3: Commit**

```bash
git add app/README.md README.md
git commit -m "docs: document the cross-platform app and its status"
```

---

## Phase 0 Acceptance

- `cargo test -p vox-core` passes (settings + secrets unit tests).
- `cargo build -p vox` and `npm --prefix ui run build` succeed on macOS,
  Windows, and Linux (verified by **App CI**).
- The app launches on each OS, shows a tray icon, opens a settings window, and
  round-trips a setting to disk and a secret to the OS keychain.

## Self-Review Notes

- **Spec coverage:** This plan implements the spec's Phase 0 scope (scaffold,
  settings store, secrets, tray + settings window, 3-OS CI). Rewrite, speech,
  TTS, hotkeys, and text I/O are intentionally out of scope and addressed by the
  Phase 1–5 plans.
- **Type consistency:** `Settings`, `SettingsStore::at/load/save`,
  `default_settings_path`, `SecretStore::{set,get,delete}`, `KeyringStore`, and
  the four Tauri commands (`load_settings`, `save_settings`, `set_secret`,
  `has_secret`) are referenced consistently across Rust and the UI.
- **No placeholders:** every code/command step contains concrete content.
