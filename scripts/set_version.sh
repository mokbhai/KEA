#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
    echo "Usage: scripts/set_version.sh <major.minor.patch>" >&2
    exit 1
fi

VERSION="$1"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Invalid semantic version: $VERSION" >&2
    exit 1
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAURI_CONFIG="$ROOT_DIR/src-tauri/tauri.conf.json"
TAURI_MANIFEST="$ROOT_DIR/src-tauri/Cargo.toml"

node -e '
const fs = require("fs");
const file = process.argv[1];
const version = process.argv[2];
const cfg = JSON.parse(fs.readFileSync(file, "utf8"));
cfg.version = version;
fs.writeFileSync(file, JSON.stringify(cfg, null, 2) + "\n");
' "$TAURI_CONFIG" "$VERSION"

perl -0pi -e 's/(^version\s*=\s*")[^"]+(")/${1}'"$VERSION"'${2}/m' "$TAURI_MANIFEST"

cd "$ROOT_DIR"
cargo metadata --format-version 1 >/dev/null

echo "Updated KEA version to $VERSION"
