#!/usr/bin/env bash
#
# Combine the per-platform updater fragments written by package_release.sh into
# the single latest.json the updater client fetches.
#
# Each platform is bundled on its own runner and can only sign for itself, so
# package_release.sh writes `dist/updater/<os>-<arch>.json` rather than a whole
# manifest. Without this merge step the last runner to upload would publish a
# latest.json listing only its own platform, and every other platform's client
# would conclude there was no update for it — a silent failure that looks
# exactly like "no new version" rather than like a broken release.
#
# Usage: scripts/merge_updater_manifest.sh <fragments-dir> <output-json> <version>
set -euo pipefail

FRAGMENTS_DIR="${1:?fragments directory required}"
OUTPUT="${2:?output path required}"
VERSION="${3:?version required}"

if ! compgen -G "$FRAGMENTS_DIR/*.json" > /dev/null; then
    echo "No updater fragments in $FRAGMENTS_DIR; skipping $OUTPUT."
    exit 0
fi

mkdir -p "$(dirname "$OUTPUT")"

# Single-quoted on purpose: the script below is JavaScript, and its `${...}`
# are template literals for node to expand, not parameters for bash to. Values
# are passed as argv instead, so nothing needs interpolating on the way in.
# shellcheck disable=SC2016
node -e '
  const fs = require("node:fs");
  const path = require("node:path");
  const [dir, out, version] = process.argv.slice(1);

  const platforms = {};
  for (const file of fs.readdirSync(dir).filter((f) => f.endsWith(".json")).sort()) {
    const fragment = JSON.parse(fs.readFileSync(path.join(dir, file), "utf8"));
    for (const [key, value] of Object.entries(fragment)) {
      // A duplicate key means two runners claimed the same os-arch, which is a
      // matrix misconfiguration rather than something to resolve silently by
      // last-write-wins: one of the two signatures would then be published
      // against a binary it does not actually sign.
      if (key in platforms) {
        throw new Error(`duplicate platform key ${key} (from ${file})`);
      }
      if (!value || typeof value.signature !== "string" || typeof value.url !== "string") {
        throw new Error(`fragment ${file} has no signature/url for ${key}`);
      }
      platforms[key] = { signature: value.signature, url: value.url };
    }
  }

  if (Object.keys(platforms).length === 0) {
    throw new Error(`no platform entries found in ${dir}`);
  }

  fs.writeFileSync(
    out,
    JSON.stringify(
      {
        version: `v${version}`,
        notes: `KEA ${version}`,
        pub_date: new Date().toISOString().replace(/\.\d{3}Z$/, "Z"),
        platforms,
      },
      null,
      2,
    ) + "\n",
  );
  console.log(`Merged ${Object.keys(platforms).length} platform(s) into ${out}: ${Object.keys(platforms).join(", ")}`);
' "$FRAGMENTS_DIR" "$OUTPUT" "$VERSION"
