#!/usr/bin/env bash
# Stop: do not let a run end with unchecked plan items. Inert unless a run is
# active with plan_open > 0. One-time-block: the FIRST stop with open items is
# blocked (exit 2); a per-session flag lets the SECOND stop through. The flag
# clears at SessionEnd. Exit 2 to block, 0 otherwise.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

state="$(hook_active_state)" || exit 0
blob="$(cat "$state" 2>/dev/null)"

open="$(json_num plan_open "$blob")"
[ -n "$open" ] || open=0
# No open plan items: nothing to block.
[ "$open" -gt 0 ] 2>/dev/null || exit 0

flag="$(hook_state_dir)/plan-completion.warned"
if [ -f "$flag" ]; then
  exit 0
fi
mkdir -p "$(hook_state_dir)" 2>/dev/null || true
: >"$flag" 2>/dev/null || true
hook_block "Blocked (once): $open plan item(s) still unchecked. Complete them or stop again to override."
