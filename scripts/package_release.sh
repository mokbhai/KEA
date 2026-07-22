#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DIST_DIR="$ROOT_DIR/dist"
APP_NAME="KEA"

cd "$ROOT_DIR"

VERSION="$(./scripts/current_version.sh)"
ZIP_PATH="$DIST_DIR/${APP_NAME}-${VERSION}.zip"

make clean
make build

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
rm -f "$DIST_DIR"/"${APP_NAME}"-*.zip "$DIST_DIR"/"${APP_NAME}"_*.dmg "$DIST_DIR"/"${APP_NAME}"-*.dmg

ditto -c -k --sequesterRsrc --keepParent "$APP_PATH" "$ZIP_PATH"

while IFS= read -r artifact; do
    cp "$artifact" "$DIST_DIR/"
done < <(find "$ROOT_DIR/target/release/bundle" "$ROOT_DIR/src-tauri/target/release/bundle" -type f \( -name "${APP_NAME}_${VERSION}_*.dmg" -o -name "${APP_NAME}_${VERSION}.dmg" -o -name "${APP_NAME}-${VERSION}.dmg" \) 2>/dev/null || true)

echo "Created release artifacts:"
echo "  $ZIP_PATH"
find "$DIST_DIR" -maxdepth 1 -type f \( -name "${APP_NAME}_*.dmg" -o -name "${APP_NAME}-*.dmg" \) -print | sed 's/^/  /'
