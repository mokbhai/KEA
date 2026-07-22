#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TAURI_CONFIG="$ROOT_DIR/src-tauri/tauri.conf.json"

VERSION="$(node -e 'const fs = require("fs"); const cfg = JSON.parse(fs.readFileSync(process.argv[1], "utf8")); process.stdout.write(cfg.version || "");' "$TAURI_CONFIG")"

if [[ -z "${VERSION:-}" ]]; then
    echo "Could not determine version from $TAURI_CONFIG" >&2
    exit 1
fi

echo "$VERSION"
