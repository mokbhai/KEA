#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

usage() {
    echo "Usage: scripts/release.sh <major.minor.patch>"
    exit 1
}

if [[ $# -ne 1 ]]; then
    usage
fi

VERSION="$1"
TAG="v$VERSION"
DATE="$(date +%F)"

if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "Version must use semantic versioning (for example: 1.5.0)." >&2
    exit 1
fi

cd "$ROOT_DIR"

DIRTY_FILES="$(git status --porcelain | awk '{print $2}')"
if [[ -n "$DIRTY_FILES" ]]; then
    while IFS= read -r file; do
        if [[ -n "$file" && "$file" != "CHANGELOG.md" ]]; then
            echo "Working tree contains unrelated changes: $file" >&2
            echo "Commit or stash them before starting a release." >&2
            exit 1
        fi
    done <<< "$DIRTY_FILES"
fi

if git rev-parse "$TAG" >/dev/null 2>&1; then
    echo "Git tag $TAG already exists." >&2
    exit 1
fi

if ! grep -q "^## \[$VERSION\] - $DATE$" CHANGELOG.md; then
    echo "Add a CHANGELOG entry for $VERSION dated $DATE before releasing." >&2
    exit 1
fi

./scripts/set_version.sh "$VERSION"
make release-check
./scripts/package_release.sh

git add CHANGELOG.md src-tauri/tauri.conf.json src-tauri/Cargo.toml Cargo.lock
git commit -m "chore: release $TAG"
git tag -a "$TAG" -m "Release $TAG"

echo "Release commit and tag created."
echo "Next steps:"
echo "  git push origin main"
echo "  git push origin $TAG"
