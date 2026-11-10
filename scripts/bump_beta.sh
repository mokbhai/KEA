#!/usr/bin/env bash
# Picks the version an install carries, so every build is identifiable.
#
# With no argument, moves to the next beta:
#   0.5.5          -> 0.5.6-beta.1   (first build after a release)
#   0.5.6-beta.1   -> 0.5.6-beta.2
#
# With an argument (`make install VERSION=...`), sets exactly that version and
# bumps nothing. A release is a plain number, a beta may be written either way:
#   0.5.6      0.5.7-beta.3      0.5.7b3
#
# Stored as semver (`-beta.N`) because Cargo and Tauri require it, and so the
# updater orders 0.5.6-beta.2 before 0.5.6. The app shows it as 0.5.6b2.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CURRENT="$(node -p 'require(process.argv[1]).version' "$ROOT_DIR/src-tauri/tauri.conf.json")"
REQUESTED="${1:-}"

if [[ -n "$REQUESTED" ]]; then
    # 0.5.7b3 is how a beta is written by hand; semver wants 0.5.7-beta.3.
    if [[ "$REQUESTED" =~ ^([0-9]+\.[0-9]+\.[0-9]+)b([1-9][0-9]*)$ ]]; then
        NEXT="${BASH_REMATCH[1]}-beta.${BASH_REMATCH[2]}"
    else
        NEXT="$REQUESTED"
    fi
elif [[ "$CURRENT" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)-beta\.([0-9]+)$ ]]; then
    NEXT="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.${BASH_REMATCH[3]}-beta.$((BASH_REMATCH[4] + 1))"
elif [[ "$CURRENT" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    NEXT="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.$((BASH_REMATCH[3] + 1))-beta.1"
else
    echo "Unrecognised version in tauri.conf.json: $CURRENT" >&2
    exit 1
fi

if [[ "$NEXT" == "$CURRENT" ]]; then
    echo "Version $CURRENT (unchanged)"
    exit 0
fi

echo "Version $CURRENT -> $NEXT"
"$ROOT_DIR/scripts/set_version.sh" "$NEXT"
