#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: scripts/extract_release_notes.sh <version>" >&2
    exit 1
fi

VERSION="${1#v}"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CHANGELOG_FILE="$ROOT_DIR/CHANGELOG.md"

NOTES="$(
    awk -v version="$VERSION" '
        $0 ~ "^## \\[" version "\\] - " { collecting = 1; next }
        collecting && /^## \[/ { exit }
        collecting { print }
    ' "$CHANGELOG_FILE"
)"

if [[ -z "${NOTES//[$'\t\r\n ']}" ]]; then
    echo "Release notes for $VERSION were not found in $CHANGELOG_FILE" >&2
    exit 1
fi

printf '%s\n' "$NOTES"
