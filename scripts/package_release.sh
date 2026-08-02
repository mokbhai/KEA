#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"
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
ZIP_PATH="$DIST_DIR/${APP_NAME}-${VERSION}.zip"

# `--no-build` packages a bundle a previous step already produced. The release
# workflow uses it so the tag build runs exactly once: `make clean` + a rebuild
# here would delete the very artifacts (including the signed updater tarball)
# that the build step just created, and the second full Tauri build is what
# pushed the job past its timeout.
if [[ "$BUILD" -eq 1 ]]; then
    make clean
    # Routed through `make build` rather than calling the CLI directly so the
    # signed/--no-sign selection in the Makefile applies here too.
    if [[ -n "$FEATURES" ]]; then
        make build TAURI_BUILD_FLAGS="--features $FEATURES"
    else
        make build
    fi
fi

# `find` exits non-zero when one of the search paths is absent (this is a cargo
# workspace, so only target/ exists, never src-tauri/target/). Under
# `set -o pipefail` + `set -e` that non-zero status aborts the script inside the
# command substitution — before the guard below can report anything. `|| true`
# keeps the lookup best-effort; the emptiness check below is the real guard.
APP_PATH="$(find "$ROOT_DIR/target/release/bundle/macos" "$ROOT_DIR/src-tauri/target/release/bundle/macos" -maxdepth 1 -name "${APP_NAME}.app" -type d 2>/dev/null | head -n 1 || true)"
if [[ -z "$APP_PATH" || ! -d "$APP_PATH" ]]; then
    echo "Expected Tauri app bundle named ${APP_NAME}.app under target/release/bundle/macos or src-tauri/target/release/bundle/macos" >&2
    exit 1
fi

mkdir -p "$DIST_DIR"
# --no-build skips `make clean`, so artifacts from an earlier version can still
# be sitting in dist/. Clear them explicitly rather than publishing a mix.
rm -f "$DIST_DIR"/"${APP_NAME}"-*.zip \
      "$DIST_DIR"/"${APP_NAME}"_*.dmg \
      "$DIST_DIR"/"${APP_NAME}"-*.dmg \
      "$DIST_DIR"/"${APP_NAME}"_*.app.tar.gz \
      "$DIST_DIR"/"${APP_NAME}"_*.app.tar.gz.sig \
      "$DIST_DIR"/latest.json

ditto -c -k --sequesterRsrc --keepParent "$APP_PATH" "$ZIP_PATH"

while IFS= read -r artifact; do
    cp "$artifact" "$DIST_DIR/"
done < <(find "$ROOT_DIR/target/release/bundle" "$ROOT_DIR/src-tauri/target/release/bundle" -type f \( -name "${APP_NAME}_${VERSION}_*.dmg" -o -name "${APP_NAME}_${VERSION}.dmg" -o -name "${APP_NAME}-${VERSION}.dmg" \) 2>/dev/null || true)

echo "Created release artifacts:"
echo "  $ZIP_PATH"
find "$DIST_DIR" -maxdepth 1 -type f \( -name "${APP_NAME}_*.dmg" -o -name "${APP_NAME}-*.dmg" \) -print | sed 's/^/  /'

# Updater artifacts exist only when bundle.updater.pubkey is set,
# bundle.createUpdaterArtifacts is on, and TAURI_SIGNING_PRIVATE_KEY was in the
# environment for the build. Absent any of those this is a normal unsigned
# release, so the manifest is skipped rather than treated as a failure.
TAR_GZ="$(find "$ROOT_DIR/target/release/bundle/macos" "$ROOT_DIR/src-tauri/target/release/bundle/macos" -maxdepth 1 -name '*.app.tar.gz' 2>/dev/null | head -n 1 || true)"
SIG_FILE="$(find "$ROOT_DIR/target/release/bundle/macos" "$ROOT_DIR/src-tauri/target/release/bundle/macos" -maxdepth 1 -name '*.app.tar.gz.sig' 2>/dev/null | head -n 1 || true)"

if [[ -z "$TAR_GZ" || -z "$SIG_FILE" ]]; then
    echo "No signed updater artifacts produced; skipping latest.json."
    exit 0
fi

ARCH="$(uname -m)"
case "$ARCH" in
    arm64) UPDATER_ARCH="aarch64" ;;
    *) UPDATER_ARCH="$ARCH" ;;
esac

OUT_TAR="${APP_NAME}_${VERSION}_${UPDATER_ARCH}.app.tar.gz"
cp "$TAR_GZ" "$DIST_DIR/$OUT_TAR"
cp "$SIG_FILE" "$DIST_DIR/${OUT_TAR}.sig"

# The updater client verifies the CONTENTS of the .sig file (base64 ed25519
# emitted by the Tauri signer), not a digest computed here. Never hand-roll it.
SIG="$(cat "$SIG_FILE")"

cat > "$DIST_DIR/latest.json" << JSONEOF
{
  "version": "v${VERSION}",
  "notes": "${APP_NAME} ${VERSION}",
  "pub_date": "$(date -u +"%Y-%m-%dT%H:%M:%SZ")",
  "platforms": {
    "darwin-${UPDATER_ARCH}": {
      "signature": "${SIG}",
      "url": "https://github.com/${REPO_SLUG}/releases/download/v${VERSION}/${OUT_TAR}"
    }
  }
}
JSONEOF

echo "  $DIST_DIR/$OUT_TAR"
echo "  $DIST_DIR/latest.json"
