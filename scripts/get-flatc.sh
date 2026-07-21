#!/usr/bin/env bash
# Downloads the pinned flatc 25.2.10 Linux binary into .tools/flatc and verifies
# it against contracts/flatc.sha256. Idempotent: no-ops when a matching flatc is
# already present. Parity mirror: scripts/get-flatc.ps1 (Windows).
set -euo pipefail

FORCE=0
[ "${1:-}" = "--force" ] && FORCE=1

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

VERSION="25.2.10"
ASSET="Linux.flatc.binary.clang++-18.zip"
URL="https://github.com/google/flatbuffers/releases/download/v${VERSION}/${ASSET}"
TOOLS_DIR="${REPO_ROOT}/.tools/flatc"
EXE_PATH="${TOOLS_DIR}/flatc"
ZIP_PATH="${TOOLS_DIR}/${ASSET}"
SHA256_FILE="${REPO_ROOT}/contracts/flatc.sha256"

expected_hash() {
    local asset="$1"
    [ -f "$SHA256_FILE" ] || { echo "Checksum file not found: $SHA256_FILE" >&2; exit 1; }
    # Format per line: "<sha256>  <filename>"
    local hash
    hash="$(awk -v a="$asset" '$2 == a { print $1; exit }' "$SHA256_FILE")"
    if [ -z "$hash" ]; then
        echo "No checksum entry for '$asset' in $SHA256_FILE" >&2
        echo "Add the '$asset' sha256 line to contracts/flatc.sha256 before running on Linux." >&2
        exit 1
    fi
    echo "$hash" | tr '[:upper:]' '[:lower:]'
}

# Idempotency: existing flatc reporting the pinned version is a no-op.
if [ -x "$EXE_PATH" ] && [ "$FORCE" -eq 0 ]; then
    if "$EXE_PATH" --version 2>/dev/null | grep -qF "$VERSION"; then
        echo "flatc $VERSION already present at $EXE_PATH"
        exit 0
    fi
fi

mkdir -p "$TOOLS_DIR"
EXPECTED="$(expected_hash "$ASSET")"

echo "Downloading $URL"
if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$URL" -o "$ZIP_PATH"
elif command -v wget >/dev/null 2>&1; then
    wget -qO "$ZIP_PATH" "$URL"
else
    echo "Neither curl nor wget is available" >&2
    exit 1
fi

if command -v sha256sum >/dev/null 2>&1; then
    ACTUAL="$(sha256sum "$ZIP_PATH" | awk '{print $1}')"
else
    ACTUAL="$(shasum -a 256 "$ZIP_PATH" | awk '{print $1}')"
fi
ACTUAL="$(echo "$ACTUAL" | tr '[:upper:]' '[:lower:]')"

if [ "$ACTUAL" != "$EXPECTED" ]; then
    rm -f "$ZIP_PATH"
    echo "SHA-256 mismatch for ${ASSET}: expected $EXPECTED, got $ACTUAL" >&2
    exit 1
fi
echo "SHA-256 verified: $ACTUAL"

rm -f "$EXE_PATH"
if command -v unzip >/dev/null 2>&1; then
    unzip -oq "$ZIP_PATH" -d "$TOOLS_DIR"
else
    echo "unzip is required to extract $ASSET" >&2
    exit 1
fi
chmod +x "$EXE_PATH"

[ -x "$EXE_PATH" ] || { echo "flatc not found after extracting $ASSET" >&2; exit 1; }
echo "Installed: $("$EXE_PATH" --version)"
echo "flatc ready at $EXE_PATH"
