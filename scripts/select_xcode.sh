#!/usr/bin/env bash
# Selects the newest Xcode 26 on a GitHub macOS runner.
#
# screencapturekit pulls in apple-metal, whose Swift bridge uses Metal API
# (MTLSamplerReductionMode) that first shipped in the Xcode 26 SDK. The
# macos-15 image installs Xcode 26 but defaults to 16.x, so without this the
# system-audio-sck build fails inside apple-metal's build script.
set -euo pipefail

XCODE="$(ls -d /Applications/Xcode_26*.app 2>/dev/null | sort -V | tail -1 || true)"
if [[ -z "$XCODE" ]]; then
    echo "No Xcode 26 on this runner; system-audio-sck needs its SDK." >&2
    exit 1
fi
sudo xcode-select -s "$XCODE"
xcodebuild -version
