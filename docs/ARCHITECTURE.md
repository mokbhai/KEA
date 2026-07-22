# Architecture

KEA is a Tauri 2.x desktop app with a React/Vite frontend and a Rust workspace backend. The Tauri crate is intentionally thin: it wires platform services, engine registries, feature orchestration, persistence, commands, events, tray state, and windows together.

## Top-Level Structure

### `src-tauri/`

- `src/main.rs`
  Tauri entry point, plugin setup, tray/menu setup, command registration, state construction, and hotkey routing.
- `src/commands.rs`
  Tauri command wrappers around stores, features, engines, settings, permissions, and model management.
- `src/events.rs`
  Event emission helpers for long-running workflows and UI status updates.
- `tauri.conf.json`
  Product identity, bundle identifier, dev URL, frontend build commands, bundle targets, windows, and tray icon.

### `ui/`

- `src/main.tsx`
  React app entry point.
- `src/App.tsx`
  Main UI shell.
- `package.json`
  Vite, TypeScript, React, and Tauri API scripts/dependencies.

### `crates/core/`

- Cross-platform domain types
- Plugin traits and registries
- Settings, bindings, credential, action, and history storage contracts
- Shared error and event types

### `crates/features/`

- Rewrite feature orchestration
- Dictation feature orchestration
- Meeting workflow orchestration
- Text-to-speech orchestration

### `crates/engines/`

- Hosted engine integrations
- OpenAI-compatible clients
- Local engine adapters that do not belong in platform code

### `crates/platform/`

- Global hotkeys
- Clipboard and text insertion
- Audio capture and playback
- Permission checks and platform-specific behavior

### `crates/infer/`

- Optional local inference helpers
- Model discovery, download, and adapter glue

## Core Flows

### Tauri Startup Flow

1. `src-tauri/src/main.rs` builds stores, registries, platform adapters, and feature services.
2. Tauri plugins such as autostart and notification are registered.
3. Commands are exposed to the React UI.
4. Tray and window behavior is configured.
5. Global hotkey listeners route user actions into the relevant feature orchestration.

### Rewrite Flow

1. A hotkey or UI command requests a rewrite.
2. Platform text I/O captures the selected text.
3. The rewrite feature resolves the configured LLM engine and prompt/preset.
4. The engine returns the rewritten text.
5. Platform text I/O inserts the result and the action is recorded.

### Dictation Flow

1. A hotkey or UI command starts audio capture.
2. The dictation feature resolves the configured STT engine.
3. Audio is transcribed locally or through a hosted provider.
4. The transcript is inserted into the active app or surfaced in the UI.

### Meeting Flow

1. Meeting capture gathers audio according to platform permission support.
2. Audio is chunked, transcribed, and optionally diarized.
3. The meeting feature synthesizes notes and follow-up artifacts.
4. State and history are persisted through the configured stores.

### Release Flow

1. `src-tauri/tauri.conf.json` and `src-tauri/Cargo.toml` carry the application version.
2. `cargo tauri build` creates platform bundles.
3. `scripts/package_release.sh` copies bundle artifacts into `dist/`.
