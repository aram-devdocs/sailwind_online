#!/usr/bin/env bash
# Regenerates the committed FlatBuffers bindings (C# + Rust) from
# contracts/fbs/envelope.fbs using the pinned flatc in .tools/flatc.
# Reproduces the committed output byte-for-byte; CI diffs afterward.
# Parity mirror: scripts/gen-contracts.ps1 (Windows).
# Ensure flatc is present first: scripts/get-flatc.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# Prefer the platform binary name; fall back to flatc.exe (Windows/Git-Bash).
FLATC="${REPO_ROOT}/.tools/flatc/flatc"
[ -x "$FLATC" ] || FLATC="${REPO_ROOT}/.tools/flatc/flatc.exe"

SCHEMA="${REPO_ROOT}/contracts/fbs/envelope.fbs"
CS_OUT="${REPO_ROOT}/contracts/cs/Sailwind.Contracts/Generated"
RUST_OUT="${REPO_ROOT}/server/crates/sw-contracts/src/generated"

if [ ! -x "$FLATC" ]; then
    echo "flatc not found under ${REPO_ROOT}/.tools/flatc. Run scripts/get-flatc.sh first." >&2
    exit 1
fi
[ -f "$SCHEMA" ] || { echo "Schema not found: $SCHEMA" >&2; exit 1; }

mkdir -p "$CS_OUT" "$RUST_OUT"

echo "flatc: $("$FLATC" --version)"

echo "Generating C# -> $CS_OUT"
"$FLATC" --csharp --gen-all -o "$CS_OUT" "$SCHEMA"

echo "Generating Rust -> $RUST_OUT"
"$FLATC" --rust --gen-all -o "$RUST_OUT" "$SCHEMA"

echo "Contracts regenerated."
