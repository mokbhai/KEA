# Release

KEA releases are built with Tauri from the Rust workspace and React UI.

## Version Source Of Truth

The application version is stored in:

- `src-tauri/tauri.conf.json`
- `src-tauri/Cargo.toml`

Use the helper script to update both together:

```bash
./scripts/set_version.sh 0.1.0
```

## Release Checklist

1. Add the dated changelog entry in `CHANGELOG.md`.
2. Update the app version with `./scripts/set_version.sh <version>`.
3. Run the verification gate:

```bash
make release-check
```

This runs TypeScript checks, rustfmt, Rust compile checks, Rust tests, UI build, and a Tauri bundle build.

4. Build release artifacts:

```bash
./scripts/package_release.sh
```

5. Create the release commit and annotated tag:

```bash
./scripts/release.sh 0.1.0
```

## Release Artifacts

`scripts/package_release.sh` produces artifacts under `dist/`, including:

- `dist/KEA-<version>.zip`
- Tauri-generated DMG files such as `dist/KEA_<version>_<arch>.dmg`

Artifacts are copied from the Tauri bundle output under `target/release/bundle/` or `src-tauri/target/release/bundle/`.

## Auto-Update

KEA supports in-app updates via `tauri-plugin-updater`. By default, the updater is **inactive** — the `updater` Cargo feature is off and the public key in `tauri.conf.json` is empty. The app builds and runs normally without it.

### Activating Updates

You need a signing keypair for the updater to verify release integrity:

#### 1. Generate a keypair

```bash
cargo tauri signer generate -w ~/.tauri/kea-updater.key
```

This produces a public/private key pair saved at `~/.tauri/kea-updater.key`.

#### 2. Copy the public key

```bash
cargo tauri signer generate -w ~/.tauri/kea-updater.key --public
```

Open `src-tauri/tauri.conf.json` and replace the empty `"pubkey"` value with this key:

```json
"plugins": {
  "updater": {
    "pubkey": "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6I...",
    "endpoints": [
      "https://github.com/mokbhai/vox/releases/latest/download/latest.json"
    ]
  }
}
```

#### 3. Store the private key as a GitHub secret

Copy the private key from `~/.tauri/kea-updater.key` and add it as a repository secret:

- **`TAURI_SIGNING_PRIVATE_KEY`**: the full contents of the key file.
- **`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`** (optional): password if you encrypted the key.

Without these secrets, the release workflow skips updater artifact generation but still publishes the normal `.zip` and `.dmg` artifacts.

#### 4. Enable updater artifact generation

Set `bundle.createUpdaterArtifacts: true` in `src-tauri/tauri.conf.json` and build with the `updater` feature:

```bash
cargo tauri build --features updater
```

With the pubkey set, the signing secret in the environment, and `createUpdaterArtifacts` on, the Tauri bundler produces the updater archive (`*.app.tar.gz`) **and its ed25519 signature file** (`*.app.tar.gz.sig`) automatically. The release workflow already runs this signed build when `TAURI_SIGNING_PRIVATE_KEY` is set and copies the `.sig` contents into `latest.json`.

> Do **not** sign the tarball by hand (e.g. `openssl dgst`): Tauri verifies an ed25519 signature produced by its own signer, and any other algorithm is rejected by every client. The workflow reads the bundler's `.sig` file directly.

### How latest.json Works

The release workflow (`release.yml`) generates `latest.json` from the bundler's signature when `TAURI_SIGNING_PRIVATE_KEY` is present. This manifest is published to:

```
https://github.com/mokbhai/vox/releases/latest/download/latest.json
```

The manifest structure:

```json
{
  "version": "v0.1.0",
  "notes": "KEA 0.1.0",
  "pub_date": "2026-01-01T00:00:00Z",
  "platforms": {
    "darwin-aarch64": {
      "signature": "<ed25519-signature-from-.sig-file>",
      "url": "https://github.com/mokbhai/vox/releases/download/v0.1.0/KEA_0.1.0_aarch64.app.tar.gz"
    }
  }
}
```

The updater plugin fetches this JSON, compares the version, downloads the `.app.tar.gz`, verifies the signature against the embedded public key, and installs the update.

### Offline / No-Key Behavior

- **Default build** (`cargo build -p kea-app`, `cargo tauri dev`, `cargo tauri build`): the updater plugin is not even compiled. The `check_update` Tauri command returns `status: "disabled"` with a message explaining the feature is off. The **Check for updates** button in the UI shows this message gracefully.
- **Build with `--features updater` but empty pubkey**: the plugin initializes but checks will fail because no valid signature verification is possible. This is intentional — fill the pubkey before shipping.
- **Offline / network error during check**: errors are logged at `warn` level. The launch-time check silently skips. The manual **Check for updates** button shows the error to the user.
- **Auto-check disabled** (`updates.auto_check = false` in settings): the launch-time check does not run. The manual button still works.
