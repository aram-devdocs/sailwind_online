#!/usr/bin/env bash
# SessionEnd: clear the per-session flags so the one-time-block Stop hooks reset
# for the next session. Always exit 0.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

state_dir="$(hook_state_dir)"
rm -f "$state_dir/completion-check.warned" "$state_dir/plan-completion.warned" 2>/dev/null || true
rm -f "$state_dir/session.json" 2>/dev/null || true
echo "state-cleanup: session flags cleared."
exit 0
