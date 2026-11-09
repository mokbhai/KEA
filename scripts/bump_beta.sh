#!/usr/bin/env bash
# Moves the app version to the next beta, so every install is identifiable.
#
#   0.5.5          -> 0.5.6-beta.1   (first build after a release)
#   0.5.6-beta.1   -> 0.5.6-beta.2
#
# Stored as semver (`-beta.N`) because Cargo and Tauri require it, and so the
# updater orders 0.5.6-beta.2 before 0.5.6. The app shows it as 0.5.6b2.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CURRENT="$(node -p 'require(process.argv[1]).version' "$ROOT_DIR/src-tauri/tauri.conf.json")"

if [[ "$CURRENT" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)-beta\.([0-9]+)$ ]]; then
    NEXT="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.${BASH_REMATCH[3]}-beta.$((BASH_REMATCH[4] + 1))"
elif [[ "$CURRENT" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    NEXT="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.$((BASH_REMATCH[3] + 1))-beta.1"
else
    echo "Unrecognised version in tauri.conf.json: $CURRENT" >&2
    exit 1
fi

echo "Version $CURRENT -> $NEXT"
"$ROOT_DIR/scripts/set_version.sh" "$NEXT"
