#!/usr/bin/env bash
# Leak guard. This repo is public; nothing proprietary may enter it. This script
# contains NO sensitive terms itself — it loads a blocklist and scans the tracked
# tree for any of them, failing on a hit.
#
# Blocklist resolution order:
#   1. $SW_LEAK_BLOCKLIST                      (explicit path, e.g. CI writes the secret here)
#   2. .agents/local/leak-blocklist.txt        (gitignored, local default)
# If neither exists, the scan is skipped with a notice (so an external contributor
# without the list is not blocked). On the maintainer's machine and in CI (with the
# LEAK_BLOCKLIST secret) the list is present and the guard is active.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

blocklist="${SW_LEAK_BLOCKLIST:-.agents/local/leak-blocklist.txt}"
if [ ! -f "$blocklist" ]; then
  echo "check-no-leak: no blocklist at '$blocklist' — scan skipped (set SW_LEAK_BLOCKLIST or add .agents/local/leak-blocklist.txt)." >&2
  exit 0
fi

hits=0
while IFS= read -r term; do
  term="${term%%$'\r'}"
  case "$term" in ''|\#*) continue ;; esac
  # Scan tracked files only; never scan the gitignored local blocklist itself.
  if matches="$(git grep -I -i -l -e "$term" -- ':!.agents/local' 2>/dev/null)"; then
    if [ -n "$matches" ]; then
      echo "check-no-leak: blocked term '$term' found in:" >&2
      echo "$matches" | sed 's/^/    /' >&2
      hits=$((hits + 1))
    fi
  fi
done < "$blocklist"

if [ "$hits" -gt 0 ]; then
  echo "check-no-leak: FAILED — $hits blocked term(s) present. Remove them; this is a public repo." >&2
  exit 1
fi
echo "check-no-leak: clean."
