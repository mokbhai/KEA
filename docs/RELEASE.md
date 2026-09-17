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

1. Add the dated changelog entry in `CHANGELOG.md`. The heading must be exactly
   `## [<version>] - <YYYY-MM-DD>` with today's date — `scripts/release.sh`
   refuses to run without it, and the workflow extracts the release body from it.
2. Update the app version with `./scripts/set_version.sh <version>`.
3. Run the lint and test gate:

```bash
make lint
make test
```

This runs TypeScript checks, Rust compile checks, doc hygiene, the UI build, and
the Rust workspace tests.

4. Build and package release artifacts (this performs the bundle build):

```bash
./scripts/package_release.sh
```

5. Create the release commit and annotated tag, then push:

```bash
./scripts/release.sh <version>
git push origin main
git push origin v<version>
```

Pushing the tag is what triggers `.github/workflows/release.yml`, which rebuilds,
signs, and publishes the GitHub release.

## The release workflow

`release.yml` runs three jobs in sequence:

1. **`verify`** — checks the tag against the package version and that
   `CHANGELOG.md` has an entry for it, then uploads the extracted notes. It is
   seconds long and gates the matrix, so a mistyped tag fails before three cold
   Tauri builds start rather than after them.
2. **`build`** — a matrix over `macos-15`, `ubuntu-latest` and
   `windows-latest`, each running the tests, building a signed bundle and
   uploading its installers plus its manifest fragment. Lint runs only on the
   macOS leg (it reads the tree, not the platform); the tests run everywhere.
3. **`publish`** — merges the fragments into `latest.json`, asserts that every
   platform in the matrix is present in it, and creates the GitHub release with
   everything in `dist/`.

`publish` requires `build` in full, so a failure on any one platform publishes
nothing at all. That is intended: a release missing one platform's entry in
`latest.json` is indistinguishable, from that platform's clients, from there
being no update.

### Still unsigned: what users see

The pipeline signs **updater artifacts** with the ed25519 key in
`TAURI_SIGNING_PRIVATE_KEY`. It does **not** do OS-level code signing, which is
a separate thing and needs certificates this repo does not have:

- **macOS** — the `.dmg`/`.app` is not signed with an Apple Developer ID and not
  notarized, so Gatekeeper shows "cannot be opened because the developer cannot
  be verified". Needs an Apple Developer account, a Developer ID certificate,
  `bundle.macOS.signingIdentity` in `tauri.conf.json`, and an `xcrun notarytool`
  step.
- **Windows** — the MSI/NSIS installer is unsigned, so SmartScreen warns. Needs
  a code-signing certificate, which is a procurement item.
- **Linux** — AppImage and `.deb` carry no signature expectation, so nothing is
  missing here.

## Release Artifacts

`scripts/package_release.sh` packages whatever the host it runs on can build,
into `dist/`. It is host-aware, not cross-compiling: a macOS machine produces
the macOS artifacts and nothing else.

| Host | Installers | Updater payload |
| --- | --- | --- |
| macOS | `KEA-<version>.zip`, `KEA_<version>_<arch>.dmg` | `KEA_<version>_<arch>.app.tar.gz` + `.sig` |
| Linux | `*.AppImage`, `*.deb` | the AppImage archive + `.sig` |
| Windows | `*.msi`, `*-setup.exe` | the signed installer archive + `.sig` |

Artifacts are copied from the Tauri bundle output under `target/release/bundle/`
or `src-tauri/target/release/bundle/`.

### The updater payload is discovered, but the format is chosen

The script does not look for a payload by an exact name. It finds a `.sig` the
bundler wrote and takes whatever sits next to it as the payload, because Tauri's
documentation and its bundler source disagree about what the Linux updater
artifact is called (`*.AppImage` in the docs, `*.AppImage.tar.gz` in the
bundler) and the Windows shape has moved across 2.x releases.

**Which** signature is not left to chance, though. The bundler signs every
updater-capable bundle it produced — on Linux that is three, deb, rpm and
AppImage — while `tauri-plugin-updater` can only *install* one format per
platform:

| Platform | Installable by the updater | Preference order |
| --- | --- | --- |
| macOS | the `.app` archive | `*.app.tar.gz.sig` |
| Linux | AppImage only | `*.AppImage.tar.gz.sig`, `*.AppImage.sig` |
| Windows | MSI or NSIS | `*.msi.zip.sig`, `*.msi.sig`, `*-setup.exe.zip.sig`, `*-setup.exe.sig` |

v0.3.0 shipped a manifest pointing Linux clients at `KEA_0.3.0_amd64.deb`, which
the updater cannot apply, purely because `find` walks `bundle/deb/` before
`bundle/appimage/`. The `.deb` and `.rpm` are still published for people who
install them by hand; they are simply not what the updater is pointed at.

If the bundler signs something but none of it matches the platform's list, that
is a hard error rather than a fallback: shipping a manifest a client cannot act
on is worse than failing the release.

The payload is then renamed to `KEA_<version>_<arch>.<ext>` before publishing,
because the macOS bundler names it `KEA.app.tar.gz` — with neither version nor
architecture in it — so an aarch64 and an x86_64 build of the same release would
otherwise overwrite each other as release assets.

### Per-platform manifest fragments

Each platform is built on its own runner and can only sign for itself, so
`package_release.sh` writes `dist/updater/<os>-<arch>.json` — a fragment, not a
manifest. `scripts/merge_updater_manifest.sh` combines the fragments into the
single `latest.json` clients fetch.

Running the script locally on one machine also leaves a `dist/latest.json`
behind covering just that host, which is what makes a local
`./scripts/package_release.sh` still useful on its own.

### Build once, package once

`package_release.sh` builds the app itself. Do **not** run a separate bundle
build before it: an earlier version of the release workflow ran `make
release-check` (which bundles) and then `package_release.sh`, which began with
`make clean` and rebuilt from scratch — two full Tauri builds per release, which
is what exhausted the job timeout.

Pass `--no-build` when a build has already happened in the same job, and
`--features <list>` to forward cargo features to that build:

```bash
./scripts/package_release.sh --features updater   # build, then package
./scripts/package_release.sh --no-build           # package an existing bundle
```

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
      "https://github.com/mokbhai/KEA/releases/latest/download/latest.json"
    ]
  }
}
```

#### 3. Store the private key as a GitHub secret

Copy the private key from `~/.tauri/kea-updater.key` and add it as a repository secret:

- **`TAURI_SIGNING_PRIVATE_KEY`**: the full contents of the key file.
- **`TAURI_SIGNING_PRIVATE_KEY_PASSWORD`** (optional): password if you encrypted the key.

Without these secrets the bundler produces no signature, and the release
workflow **fails** rather than publishing: the `Verify this platform signed its
updater payload` step treats a missing fragment as an error. That is on purpose
— an unsigned release still yields perfectly good installers, so the job would
otherwise go green while shipping a `latest.json` that no client can use.

A local `./scripts/package_release.sh` without the key is a different case and
is fine: it prints `No signed updater artifacts produced` and packages the
installers only.

#### 4. Enable updater artifact generation

Set `bundle.createUpdaterArtifacts: true` in `src-tauri/tauri.conf.json` and build with the `updater` feature:

```bash
cargo tauri build --features updater
```

With the pubkey set, the signing secret in the environment, and `createUpdaterArtifacts` on, the Tauri bundler produces the updater archive (`*.app.tar.gz`) **and its ed25519 signature file** (`*.app.tar.gz.sig`) automatically. The release workflow already runs this signed build when `TAURI_SIGNING_PRIVATE_KEY` is set and copies the `.sig` contents into `latest.json`.

> Do **not** sign the tarball by hand (e.g. `openssl dgst`): Tauri verifies an ed25519 signature produced by its own signer, and any other algorithm is rejected by every client. The workflow reads the bundler's `.sig` file directly.

### How latest.json Works

The release workflow (`release.yml`) generates `latest.json` by merging one
fragment per platform, each produced from that platform's own bundler signature,
when `TAURI_SIGNING_PRIVATE_KEY` is present. This manifest is published to:

```
https://github.com/mokbhai/KEA/releases/latest/download/latest.json
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
      "url": "https://github.com/mokbhai/KEA/releases/download/v0.1.0/KEA_0.1.0_aarch64.app.tar.gz"
    },
    "linux-x86_64": { "signature": "...", "url": "..." },
    "windows-x86_64": { "signature": "...", "url": "..." }
  }
}
```

The updater plugin fetches this JSON, compares the version, downloads the `.app.tar.gz`, verifies the signature against the embedded public key, and installs the update.

### Offline / No-Key Behavior

- **Default build** (`cargo build -p kea-app`, `cargo tauri dev`, `cargo tauri build`): the updater plugin is not even compiled. The `check_update` Tauri command returns `status: "disabled"` with a message explaining the feature is off. The **Check for updates** button in the UI shows this message gracefully.
- **Build with `--features updater` but empty pubkey**: the plugin initializes but checks will fail because no valid signature verification is possible. This is intentional — fill the pubkey before shipping.
- **Offline / network error during check**: errors are logged at `warn` level. The launch-time check silently skips. The manual **Check for updates** button shows the error to the user.
- **Auto-check disabled** (`updates.auto_check = false` in settings): the launch-time check does not run. The manual button still works.
