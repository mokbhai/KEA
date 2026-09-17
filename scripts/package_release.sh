#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"
UPDATER_DIR="$DIST_DIR/updater"
APP_NAME="KEA"
# Overridable so a fork publishes updater URLs pointing at its own releases.
REPO_SLUG="${KEA_REPO_SLUG:-mokbhai/KEA}"

BUILD=1
FEATURES=""

usage() {
    echo "Usage: scripts/package_release.sh [--no-build] [--features <list>]" >&2
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build)
            BUILD=0
            shift
            ;;
        --features)
            [[ $# -ge 2 ]] || usage
            FEATURES="$2"
            shift 2
            ;;
        *)
            usage
            ;;
    esac
done

cd "$ROOT_DIR"

VERSION="$(./scripts/current_version.sh)"

# ── Host identification ──────────────────────────────────────────────
# The updater matches on `OS-ARCH` keys, where OS is one of darwin/linux/
# windows. That is Tauri's vocabulary, not uname's, so the two are mapped
# explicitly rather than lowercasing whatever the kernel happens to report.
case "$(uname -s)" in
    Darwin) HOST_OS="macos"; UPDATER_OS="darwin" ;;
    Linux) HOST_OS="linux"; UPDATER_OS="linux" ;;
    MINGW* | MSYS* | CYGWIN*) HOST_OS="windows"; UPDATER_OS="windows" ;;
    *)
        echo "Unsupported host $(uname -s): cannot package a release here." >&2
        exit 1
        ;;
esac

case "$(uname -m)" in
    arm64 | aarch64) UPDATER_ARCH="aarch64" ;;
    x86_64 | amd64) UPDATER_ARCH="x86_64" ;;
    i686 | i386) UPDATER_ARCH="i686" ;;
    armv7*) UPDATER_ARCH="armv7" ;;
    *) UPDATER_ARCH="$(uname -m)" ;;
esac

PLATFORM_KEY="${UPDATER_OS}-${UPDATER_ARCH}"

# ── Build ────────────────────────────────────────────────────────────
# `--no-build` packages a bundle a previous step already produced. The release
# workflow uses it so the tag build runs exactly once: a clean + rebuild here
# would delete the very artifacts (including the signed updater payload) that
# the build step just created, and the second full Tauri build is what pushed
# the job past its timeout.
#
# The Tauri CLI is invoked directly rather than through `make`. GNU make is not
# guaranteed on a Windows runner, and routing through it bought nothing but the
# signed/--no-sign selection, which is six lines and lives here now.
if [[ "$BUILD" -eq 1 ]]; then
    rm -rf "$ROOT_DIR/target/release/bundle" "$ROOT_DIR/src-tauri/target/release/bundle" \
           "$ROOT_DIR/ui/dist" "$DIST_DIR"

    # tauri.conf.json carries an updater pubkey, and the bundler REFUSES to
    # build when it finds one without a matching private key ("A public key has
    # been found, but no private key"). Local bundle builds must not depend on
    # the release signing key, so fall back to --no-sign when it is absent.
    # Only TAURI_SIGNING_PRIVATE_KEY is checked: the bundler's own guard names
    # that variable specifically, and TAURI_SIGNING_PRIVATE_KEY_PATH is NOT
    # enough to get past it.
    BUILD_ARGS=()
    if [[ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]]; then
        echo "No TAURI_SIGNING_PRIVATE_KEY - building with --no-sign (no updater signature)."
        BUILD_ARGS+=(--no-sign)
    else
        echo "Signing key present - building signed updater artifacts."
    fi
    if [[ -n "$FEATURES" ]]; then
        BUILD_ARGS+=(--features "$FEATURES")
    fi

    cargo tauri build "${BUILD_ARGS[@]}"
fi

# `find` exits non-zero when one of the search paths is absent (this is a cargo
# workspace, so normally only target/ exists, never src-tauri/target/). Under
# `set -o pipefail` + `set -e` that status aborts the script inside the command
# substitution, before any guard can report anything. `|| true` keeps every
# lookup below best-effort; the emptiness checks are the real guards.
BUNDLE_DIRS=("$ROOT_DIR/target/release/bundle" "$ROOT_DIR/src-tauri/target/release/bundle")

mkdir -p "$DIST_DIR" "$UPDATER_DIR"
# --no-build skips the clean, so artifacts from an earlier version can still be
# sitting in dist/. Clear them explicitly rather than publishing a mix.
rm -f "$DIST_DIR"/"${APP_NAME}"-*.zip \
      "$DIST_DIR"/"${APP_NAME}"_*.dmg \
      "$DIST_DIR"/"${APP_NAME}"-*.dmg \
      "$DIST_DIR"/"${APP_NAME}"_*.AppImage \
      "$DIST_DIR"/"${APP_NAME}"_*.deb \
      "$DIST_DIR"/"${APP_NAME}"_*.rpm \
      "$DIST_DIR"/"${APP_NAME}"_*.msi \
      "$DIST_DIR"/"${APP_NAME}"_*.exe \
      "$DIST_DIR"/"${APP_NAME}"_*.app.tar.gz \
      "$DIST_DIR"/"${APP_NAME}"_*.app.tar.gz.sig \
      "$DIST_DIR"/latest.json
rm -f "$UPDATER_DIR"/*.json

# ── Installers ───────────────────────────────────────────────────────
# One glob list per host, because the bundler only produces the formats its own
# platform can make. A missing format is a hard error rather than a shrug: a
# release that silently ships fewer installers than the last one is the failure
# mode this whole script exists to prevent.
case "$HOST_OS" in
    macos) INSTALLER_GLOBS=("*.dmg") ;;
    linux) INSTALLER_GLOBS=("*.AppImage" "*.deb") ;;
    windows) INSTALLER_GLOBS=("*.msi" "*-setup.exe") ;;
esac

copied_any=0
for glob in "${INSTALLER_GLOBS[@]}"; do
    found=0
    while IFS= read -r artifact; do
        [[ -n "$artifact" ]] || continue
        cp "$artifact" "$DIST_DIR/"
        found=1
        copied_any=1
    done < <(find "${BUNDLE_DIRS[@]}" -type f -name "$glob" 2>/dev/null || true)
    if [[ "$found" -eq 0 ]]; then
        echo "No $glob produced under target/release/bundle - the $HOST_OS bundle is incomplete." >&2
        exit 1
    fi
done

# macOS additionally ships the raw .app as a zip, which is what a user who does
# not want the disk image downloads. ditto preserves the resource forks and the
# bundle bit; `zip -r` does not, and an .app unpacked from a plain zip will not
# launch.
if [[ "$HOST_OS" == "macos" ]]; then
    APP_PATH="$(find "${BUNDLE_DIRS[@]}" -maxdepth 2 -name "${APP_NAME}.app" -type d 2>/dev/null | head -n 1 || true)"
    if [[ -z "$APP_PATH" || ! -d "$APP_PATH" ]]; then
        echo "Expected a Tauri app bundle named ${APP_NAME}.app under target/release/bundle/macos" >&2
        exit 1
    fi
    ditto -c -k --sequesterRsrc --keepParent "$APP_PATH" "$DIST_DIR/${APP_NAME}-${VERSION}.zip"
    copied_any=1
fi

[[ "$copied_any" -eq 1 ]] || { echo "No installers were packaged." >&2; exit 1; }

echo "Packaged installers for ${PLATFORM_KEY}:"
find "$DIST_DIR" -maxdepth 1 -type f -print | sed 's/^/  /'

# ── Updater payload ──────────────────────────────────────────────────
# The payload is discovered from the signature the bundler wrote rather than
# assumed from a name. Tauri's own docs and its bundler source disagree about
# what the Linux updater artifact is called (`*.AppImage` in the docs,
# `*.AppImage.tar.gz` in the bundler), and the Windows one has changed shape
# across 2.x releases. Whatever `x.sig` sits next to IS the payload, on every
# platform and every version, so that is what this reads.
SIG_FILE="$(find "${BUNDLE_DIRS[@]}" -type f -name '*.sig' 2>/dev/null | head -n 1 || true)"

if [[ -z "$SIG_FILE" ]]; then
    # Updater artifacts exist only when bundle.updater.pubkey is set,
    # createUpdaterArtifacts is on, and TAURI_SIGNING_PRIVATE_KEY was in the
    # environment for the build. Absent any of those this is a normal unsigned
    # release, so the manifest is skipped rather than treated as a failure.
    echo "No signed updater artifacts produced; skipping the updater manifest."
    exit 0
fi

PAYLOAD_FILE="${SIG_FILE%.sig}"
if [[ ! -f "$PAYLOAD_FILE" ]]; then
    echo "Found $SIG_FILE but no payload beside it at $PAYLOAD_FILE." >&2
    exit 1
fi

# Renamed to one canonical shape across platforms so that a future release
# carrying two arches of the same OS cannot collide: the macOS payload is named
# `KEA.app.tar.gz` by the bundler, with no version or arch in it at all, so an
# aarch64 and an x86_64 build would overwrite each other in the release.
# Matching is longest-suffix-first, because `.msi.zip` has to win over `.zip`.
PAYLOAD_BASE="$(basename "$PAYLOAD_FILE")"
EXT=""
for candidate in app.tar.gz AppImage.tar.gz msi.zip exe.zip nsis.zip AppImage msi exe zip; do
    if [[ "$PAYLOAD_BASE" == *".$candidate" ]]; then
        EXT="$candidate"
        break
    fi
done
if [[ -z "$EXT" ]]; then
    # Unknown shape: keep the bundler's own name rather than inventing one, and
    # say so, because the manifest URL has to match the uploaded asset exactly.
    echo "Unrecognised updater payload name '$PAYLOAD_BASE'; publishing it unrenamed." >&2
    OUT_PAYLOAD="$PAYLOAD_BASE"
else
    OUT_PAYLOAD="${APP_NAME}_${VERSION}_${UPDATER_ARCH}.${EXT}"
fi

cp "$PAYLOAD_FILE" "$DIST_DIR/$OUT_PAYLOAD"
cp "$SIG_FILE" "$DIST_DIR/${OUT_PAYLOAD}.sig"

# The updater client verifies the CONTENTS of the .sig file (base64 ed25519
# emitted by the Tauri signer), not a digest computed here. Never hand-roll it.
SIG="$(cat "$SIG_FILE")"

# A fragment, not the manifest. Each platform builds on its own runner and can
# only ever speak for itself; merge_updater_manifest.sh combines the fragments
# once every matrix leg has finished. Writing latest.json here instead is what
# would make a three-platform release publish a manifest listing one platform
# — whichever runner happened to upload last.
cat > "$UPDATER_DIR/${PLATFORM_KEY}.json" << JSONEOF
{
  "${PLATFORM_KEY}": {
    "signature": "${SIG}",
    "url": "https://github.com/${REPO_SLUG}/releases/download/v${VERSION}/${OUT_PAYLOAD}"
  }
}
JSONEOF

echo "Updater payload for ${PLATFORM_KEY}:"
echo "  $DIST_DIR/$OUT_PAYLOAD"
echo "  $UPDATER_DIR/${PLATFORM_KEY}.json"

# Convenience for a single-platform local run: a manifest covering just this
# host, so `make release-package` still leaves a usable latest.json behind. The
# release workflow overwrites it with the merged one.
./scripts/merge_updater_manifest.sh "$UPDATER_DIR" "$DIST_DIR/latest.json" "$VERSION"
