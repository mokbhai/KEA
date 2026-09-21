#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
    exit 0
fi

valid_identities() {
    { security find-identity -v -p codesigning 2>/dev/null || true; } \
        | awk '$2 ~ /^[[:xdigit:]]{40}$/ { print $2 }'
}

identities="$(valid_identities)"
identity="${KEA_CODESIGN_IDENTITY:-}"
if [[ -z "$identity" ]]; then
    identity_count="$(awk 'NF { count += 1 } END { print count + 0 }' <<<"$identities")"
    if [[ "$identity_count" -gt 1 ]]; then
        cat >&2 <<EOF
Multiple macOS code-signing identities are available. Refusing to pick one
implicitly, because changing identities also changes KEA's TCC identity.
Set KEA_CODESIGN_IDENTITY to one of the values shown by:

  security find-identity -v -p codesigning
EOF
        exit 1
    fi
    identity="$identities"
fi

if [[ -z "$identity" ]]; then
    cat >&2 <<EOF
No stable macOS code-signing identity is available.

Refusing to build an ad-hoc-signed package. An ad-hoc
signature identifies each build by its cdhash, so rebuilding invalidates KEA's
Accessibility grant and makes the global Option+Shift hold chord stop working.

Install an Apple Development certificate through Xcode, or set
KEA_CODESIGN_IDENTITY to a valid identity shown by:

  security find-identity -v -p codesigning
EOF
    exit 1
fi

if ! grep -Fxq "$identity" <<<"$identities"; then
    echo "KEA_CODESIGN_IDENTITY is not a valid code-signing identity: $identity" >&2
    echo "Choose a SHA-1 identity shown by: security find-identity -v -p codesigning" >&2
    exit 1
fi

echo "$identity"
