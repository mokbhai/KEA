#!/usr/bin/env bash
# Smoke-launch: start the built binary in the background, poll liveness for
# N seconds, and fail if it exits early or prints panic markers. Designed for
# CI (macOS only; Linux headless requires xvfb-run).
# Local devs: set SMOKE_RESET_DATA=1 to use a throwaway data dir.
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BIN="${KEA_APP_BIN:-}"
TO="${SMOKE_TIMEOUT_SECONDS:-15}"
LOG="${ROOT_DIR}/target/smoke-launch.log"

if [ -z "${BIN:-}" ]; then
  for candidate in \
    "$ROOT_DIR/target/debug/kea-app" \
    "$ROOT_DIR/target/release/kea-app" \
    "$ROOT_DIR/src-tauri/target/debug/kea-app" \
    "$ROOT_DIR/src-tauri/target/release/kea-app"; do
    if [ -x "$candidate" ]; then BIN="$candidate"; break; fi
  done
fi
if [ -z "${BIN:-}" ] || [ ! -x "$BIN" ]; then
  echo "ERROR: no kea-app binary found; set KEA_APP_BIN or build first." >&2
  exit 1
fi

echo "=== smoke: launching $BIN, timeout ${TO}s, log $LOG ==="

# The app uses ~/Library/Application Support/ai.kea.desktop for SQLite data.
# In CI we reset it so stale DBs from previous runs can't break startup.
if [ "${SMOKE_RESET_DATA:-0}" = "1" ]; then
  DATA_DIR="${HOME}/Library/Application Support/ai.kea.desktop"
  if [ -d "$DATA_DIR" ]; then
    echo "smoke: SMOKE_RESET_DATA=1 → removing $DATA_DIR"
    rm -rf "$DATA_DIR"
  fi
fi

mkdir -p "$(dirname "$LOG")"
"$BIN" > "$LOG" 2>&1 &
PID=$!

# Poll liveness: healthy app NEVER exits on its own.
for ((i=0; i<TO; i++)); do
  if ! kill -0 "$PID" 2>/dev/null; then
    wait "$PID" || true
    RC=$?
    echo "=== smoke FAIL: kea-app exited with code $RC after ${i}s ==="
    echo "---smoke-launch.log (last 80 lines)---"
    tail -80 "$LOG" || true
    echo "---end smoke-launch.log---"
    exit 1
  fi
  sleep 1
done

# Alive at deadline — check log for panic markers.
if grep -qEi 'panicked|cannot unwind|error while building KEA' "$LOG"; then
  echo "=== smoke FAIL: panic markers found in log ==="
  grep -Ei 'panicked|cannot unwind|error while building KEA' "$LOG" || true
  kill "$PID" 2>/dev/null || true
  wait "$PID" 2>/dev/null || true
  exit 1
fi

echo "=== smoke PASS: kea-app alive after ${TO}s, no panics ==="
kill "$PID" 2>/dev/null || true
wait "$PID" 2>/dev/null || true
exit 0
