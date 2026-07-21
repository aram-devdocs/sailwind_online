#!/usr/bin/env bash
# SessionStart: print a short reminder of what to read before working. This is a
# nudge, not a gate, so it always exits 0.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

echo "Before you edit:"
echo "  1. Read AGENTS.md and the .agents/rules/ that apply to your change."

if state="$(hook_active_state)"; then
  echo "  2. A /work run is ACTIVE. Read its state: $state"
else
  echo "  2. No /work run is active (manual session; enforcing hooks are inert)."
fi

echo "  3. Read .agents/lessons-learned.md for traps earlier runs already hit."
exit 0
