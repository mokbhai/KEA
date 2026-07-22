#!/bin/sh
set -eu

cd "$(git rev-parse --show-toplevel)"

failed=0

legacy_pattern='Vox''Native|Vox''Native\.xcodeproj|/Applications/Vox\.app|com\.voxapp\.rewrite|xcode''build|XC''Test|Swift-only'
legacy_active_refs="$(git grep -n -E "$legacy_pattern" -- AGENTS.md README.md docs/README.md docs/ARCHITECTURE.md docs/DEVELOPMENT.md docs/TESTING.md docs/RELEASE.md Makefile scripts .github ':!scripts/check_kea_hygiene.sh' 2>/dev/null || true)"
if [ -n "$legacy_active_refs" ]; then
  printf '%s\n' "KEA hygiene failed: active docs/tooling still reference the retired Swift/Vox runtime:"
  printf '%s\n' "$legacy_active_refs"
  failed=1
fi

python_shebangs="$(git grep -n -E '^#!.*python' -- .github scripts Makefile 2>/dev/null || true)"
if [ -n "$python_shebangs" ]; then
  printf '%s\n' "KEA hygiene failed: tooling contains Python shebangs:"
  printf '%s\n' "$python_shebangs"
  failed=1
fi

if [ "$failed" -ne 0 ]; then
  exit 1
fi

printf '%s\n' "KEA hygiene checks passed."
