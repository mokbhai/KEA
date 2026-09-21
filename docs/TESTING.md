# Testing

KEA uses Rust tests for workspace crates and TypeScript/Vite checks for the React UI.

## Full Test Command

```bash
make test
```

This runs:

```bash
npm --prefix ui run build
cargo test --workspace
```

## Lint And Compile Checks

```bash
make lint
```

This runs:

```bash
npm --prefix ui run typecheck
cargo check --workspace
./scripts/check_kea_hygiene.sh
```

Rust formatting is available as a focused check:

```bash
make fmt-check
```

## Focused Rust Tests

```bash
cargo test -p kea-core
cargo test -p kea-features
cargo test -p kea-platform
cargo test -p kea-engines
cargo test -p kea-app
cargo test rewrite
```

The macOS global hold-to-talk integration test needs a logged-in GUI session and Accessibility access for the terminal/test binaries, so ordinary headless CI leaves it ignored. Focus another app before running it; the test asserts that the observer itself is not frontmost and that the real event tap receives `Arm`, `Start`, then `Stop` from a synthetic HID ⌥⇧ hold:

```bash
cargo test -p kea-platform --test macos_hold_global \
  modifier_only_hold_triggers_while_observer_is_not_frontmost \
  -- --ignored --exact --nocapture
```

## Focused UI Checks

```bash
npm --prefix ui run typecheck
npm --prefix ui run build
```

## Release Gate

```bash
make release-check
```

This runs `make lint`, `make test`, and `make build`. `make build` requires the Tauri CLI:

```bash
cargo install tauri-cli --version "^2" --locked
```

## Verification Expectations

- Run focused Rust or UI checks while iterating.
- Run `make test` before claiming behavior changes are complete.
- Run `make release-check` before release or distribution changes.
- Run `make install` and smoke-test `/Applications/KEA.app` after changing Tauri bundle settings, icons, signing, permissions, or macOS install behavior.
