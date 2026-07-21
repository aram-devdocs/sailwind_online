#!/usr/bin/env bash
# Fail if any game IP or build binary is tracked in git. Game DLLs, executables,
# debug symbols, and anything under lib/ or .sandbox/ must never be committed.
# Runs in the pre-commit hook and in CI (one gates-all entry point).
set -euo pipefail

tracked="$(git ls-files -- \
  '*.dll' '*.exe' '*.pdb' \
  'lib/**' 'lib/*' \
  '.sandbox/**' '.sandbox/*' \
  || true)"

if [ -n "$tracked" ]; then
  echo "error: game IP or binaries are tracked in git:" >&2
  echo "$tracked" | sed 's/^/  /' >&2
  echo >&2
  echo "Remove them and confirm lib/, .sandbox/, *.dll, *.exe, *.pdb are gitignored." >&2
  exit 1
fi

echo "guard-no-game-ip: clean (no tracked game DLLs, binaries, or sandbox)."
