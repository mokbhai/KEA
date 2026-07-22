# AGENTS.md

This repository is the **KEA Tauri/Rust/React app**. The old Swift/Xcode Vox runtime is retired for active development and must not be reintroduced as the default command path.

## Documentation

Use the active documentation set in `docs/`:

| Document | Purpose |
|----------|---------|
| [docs/README.md](docs/README.md) | Documentation index and repo orientation |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Tauri shell, Rust workspace, UI ownership, and runtime flows |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Build, install, debugging, release, and platform setup workflows |
| [docs/TESTING.md](docs/TESTING.md) | UI/Rust test commands and verification guidance |
| [docs/RELEASE.md](docs/RELEASE.md) | Release checklist and artifact guidance |

## Project Overview

KEA is a Tauri 2.x desktop app that rewrites selected text in place, provides configurable rewrite hotkeys, supports dictation and meeting workflows, and exposes local/hosted engine bindings through a Rust plugin architecture.

## Codebase Layout

- `src-tauri/`
  Tauri composition root, command/event wiring, tray/window setup, app state, and platform-specific runtime integration.
- `ui/`
  React 18, TypeScript, and Vite frontend.
- `crates/core/`
  Domain types, settings, stores, events, plugin traits, and cross-platform orchestration contracts.
- `crates/features/`
  Rewrite, dictation, meeting, and TTS feature orchestration.
- `crates/engines/`
  Hosted and local engine implementations.
- `crates/platform/`
  Platform abstractions for hotkeys, text I/O, audio, permissions, and playback.
- `crates/infer/`
  Optional local inference adapters and model helpers.
- `docs/`
  Active docs plus historical design plans.

## Commands

```bash
# Install dependencies for a fresh checkout
npm --prefix ui install
cargo install tauri-cli --version "^2" --locked

# Run the Tauri development app
make dev

# Build the Tauri app bundle
make build

# Install KEA.app to /Applications on macOS
make install

# Run UI and Rust tests
make test

# Run TypeScript, Rust compile, and active-doc hygiene checks
make lint

# Package release artifacts
make release-package
```

## Development Notes

- Product name: `KEA`.
- Bundle identifier: `ai.kea.desktop`.
- Tauri config: `src-tauri/tauri.conf.json`.
- macOS install path: `/Applications/KEA.app`.
- Prefer the Makefile targets over ad hoc commands unless you need a focused Cargo, npm, or Tauri invocation.
- After changing the Tauri product name, bundle identifier, icons, signing, or bundle targets, verify both `cargo tauri build` and `make install`.

## Expectations

- Do not restore the retired Swift/Xcode command path as the default workflow.
- Do not add Python scripts, Python packaging, retired bridge code, or dual-runtime documentation for normal development.
- If you find stale active references to the retired app paths, bundle identifiers, or Xcode test commands, update or remove them as part of the work.
