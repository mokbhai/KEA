# KEA Documentation

This documentation set describes the current Tauri/Rust/React KEA repository.

## Index

| Document | Description |
|----------|-------------|
| [DEVELOPMENT.md](DEVELOPMENT.md) | Local setup, daily commands, install flow, platform dependencies, and release workflow |
| [ARCHITECTURE.md](ARCHITECTURE.md) | Tauri composition root, Rust crate ownership, UI ownership, and major runtime flows |
| [TESTING.md](TESTING.md) | UI/Rust test commands, focused checks, and verification expectations |
| [RELEASE.md](RELEASE.md) | Versioning, release checklist, and distribution artifacts |

## Recommended Reading Order

1. Read the repository root [AGENTS.md](../AGENTS.md).
2. Read [DEVELOPMENT.md](DEVELOPMENT.md) for setup, run, build, and install commands.
3. Read [ARCHITECTURE.md](ARCHITECTURE.md) before changing runtime behavior.
4. Read [TESTING.md](TESTING.md) before modifying or adding tests.

## Current Codebase Snapshot

- `src-tauri/` owns Tauri command/event wiring, app setup, tray/window behavior, and runtime composition.
- `ui/` owns the React/Vite frontend.
- `crates/core/` owns domain contracts, settings, persistence, events, and plugin traits.
- `crates/features/` owns user-facing feature orchestration.
- `crates/engines/` owns hosted and local engine implementations.
- `crates/platform/` owns platform text I/O, hotkeys, audio, permissions, and playback abstractions.
- `crates/infer/` owns optional local inference adapters and model helpers.
