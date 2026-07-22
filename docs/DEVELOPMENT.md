# Development

## Runtime

KEA is a Tauri 2.x app built from `src-tauri/`, a Rust workspace, and a React/Vite UI in `ui/`.

- Product name: `KEA`
- Bundle identifier: `ai.kea.desktop`
- macOS install path: `/Applications/KEA.app`
- Tauri config: `src-tauri/tauri.conf.json`

## Prerequisites

Install Rust, Node.js 20+, and the Tauri CLI:

```bash
cargo install tauri-cli --version "^2" --locked
npm --prefix ui install
```

On Linux, install Tauri WebKit/AppIndicator dependencies:

```bash
sudo apt-get update
sudo apt-get install -y libwebkit2gtk-4.1-dev libappindicator3-dev librsvg2-dev patchelf libasound2-dev
```

## Primary Commands

```bash
# Run the development app
make dev

# Build the Tauri app bundle
make build

# Build and install KEA.app to /Applications on macOS
make install

# Build the UI and run Rust workspace tests
make test

# Run TypeScript, Rust compile, and active-doc hygiene checks
make lint

# Check Rust formatting
make fmt-check

# Install local git hooks
make install-hooks

# Reset macOS TCC permissions for KEA
make reset-perms

# Update the Tauri/Cargo app version
make set-version VERSION=0.1.0
```

## Focused Commands

```bash
npm --prefix ui run dev
npm --prefix ui run build
npm --prefix ui run typecheck
cargo check --workspace
cargo fmt --all -- --check
cargo test --workspace
cargo test -p kea-core
cargo tauri dev
cargo tauri build
```

## Repository Layout

```text
src-tauri/
ui/
crates/
  core/
  engines/
  features/
  infer/
  platform/
docs/
scripts/
```

## macOS Install Workflow

```bash
make install
open /Applications/KEA.app
```

`make install` builds the Tauri release bundle, finds `KEA.app` under the Tauri bundle output, copies it to `/Applications`, ad-hoc signs the installed copy, and resets Accessibility, Screen Capture, and Microphone permissions for `ai.kea.desktop`.

To inspect generated bundles:

```bash
find target src-tauri/target -path "*bundle*" -maxdepth 6 2>/dev/null
```

## macOS Services Menu

KEA registers a "Rewrite with KEA" entry in the macOS Services menu (and
context menu). After a fresh install, macOS may not pick up the new service
until the pbs cache is flushed:

```bash
# After make install, tell the pasteboard server to re-scan for services
/System/Library/CoreServices/pbs -update
# Or, if that doesn't take effect:
make install && /usr/libexec/pbs -flush
```

Verify by selecting text in TextEdit, opening the app menu → Services, and
confirming "Rewrite with KEA" appears.

## Troubleshooting

**Database migration failure at startup**: If KEA exits immediately with a migration error, the `config.db` or `data.db` in the app data directory may be corrupt or from an incompatible schema version. Rename or delete the `.db` files and restart KEA to re-create them from the latest migrations. The DB paths are logged to stderr on failure; on macOS they default to `~/Library/Application Support/ai.kea.desktop/`.

## Stored Data

KEA persists configuration, credentials, actions, model metadata, and logs through the Rust stores wired in `src-tauri/src/main.rs`. Check the relevant store implementation in `crates/core/` before changing paths or schema behavior.

## Permissions And Startup

Platform permission behavior lives behind `crates/platform/` abstractions and is composed in `src-tauri/`. On macOS, reset prompts during local testing with:

```bash
make reset-perms
```

## Versioning And Release

- `src-tauri/tauri.conf.json` is the Tauri bundle version source.
- `src-tauri/Cargo.toml` carries the `kea-app` crate version.
- `scripts/set_version.sh` updates both and refreshes Cargo metadata.
- `scripts/package_release.sh` builds Tauri artifacts and copies them into `dist/`.
- `scripts/release.sh` performs the release flow and creates the git tag.

## Local Hooks

Run `make install-hooks` once per checkout to point Git at `.githooks/`.
The pre-commit hook runs `make pre-commit-check`, which uses the same release gate as CI.
