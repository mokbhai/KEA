#!/usr/bin/env bash
# Measures the size and sha256 of a model asset and prints it in the exact
# literal form `crates/infer/src/registry.rs` wants.
#
# Both fields are required on `ModelEntry` and neither can be guessed: a wrong
# sha256 fails the download at the verify step (which is the point), and a wrong
# size only shows up as a progress bar that lies. So they are measured here
# rather than copied from a release page.
#
# Usage:
#   scripts/model_checksum.sh <url> [more urls...]
#
# The download is streamed to a temp file and deleted afterwards; nothing is
# installed and no model directory is touched.
set -euo pipefail

if [[ $# -lt 1 ]]; then
    echo "Usage: scripts/model_checksum.sh <url> [more urls...]" >&2
    exit 2
fi

if command -v sha256sum >/dev/null 2>&1; then
    sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then
    sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else
    echo "need sha256sum or shasum" >&2
    exit 1
fi

# stat(1) is not portable between BSD and GNU, and this script runs on both.
if stat -f%z . >/dev/null 2>&1; then
    size_of() { stat -f%z "$1"; }
else
    size_of() { stat -c%s "$1"; }
fi

# Groups digits the way a Rust integer literal reads: 547_000_000.
group_digits() {
    echo "$1" | rev | sed 's/\([0-9]\{3\}\)/\1_/g' | rev | sed 's/^_//'
}

tmp="$(mktemp -t kea-model-checksum)"
trap 'rm -f "$tmp"' EXIT

for url in "$@"; do
    name="${url##*/}"
    name="${name%%\?*}"
    echo "Fetching $name ..." >&2
    curl -fsSL --retry 3 --retry-delay 2 -o "$tmp" "$url"

    bytes="$(size_of "$tmp")"
    sha="$(sha256_of "$tmp")"

    echo
    echo "// $name"
    echo "url: \"$url\""
    echo "    .into(),"
    echo "size_bytes: $(group_digits "$bytes"),"
    echo "sha256: \"$sha\""
    echo "    .into(),"
    echo
done
