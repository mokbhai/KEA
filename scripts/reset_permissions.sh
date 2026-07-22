#!/usr/bin/env bash
set -euo pipefail

BUNDLE_ID="ai.kea.desktop"
APP_NAME="KEA"

echo "Resetting permissions for $APP_NAME..."

if pgrep -x "$APP_NAME" >/dev/null 2>&1; then
    echo "Quitting $APP_NAME..."
    pkill -x "$APP_NAME" 2>/dev/null || true
    sleep 1
fi

echo "Resetting Accessibility permissions..."
tccutil reset Accessibility "$BUNDLE_ID" 2>/dev/null || true

echo "Resetting Screen Capture permissions..."
tccutil reset ScreenCapture "$BUNDLE_ID" 2>/dev/null || true

echo "Resetting Microphone permissions..."
tccutil reset Microphone "$BUNDLE_ID" 2>/dev/null || true

echo "Resetting all remaining TCC permissions..."
tccutil reset All "$BUNDLE_ID" 2>/dev/null || true

echo "Permissions reset complete."
