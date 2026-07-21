#!/usr/bin/env bash
# Stop: do not let a run end with required review gates unfinished. Inert unless
# a run is active with at least one gate lacking a verdict. One-time-block: the
# FIRST stop with outstanding gates is blocked (exit 2) with the list; a
# per-session flag then lets the SECOND stop through. The flag clears at
# SessionEnd. Exit 2 to block, 0 otherwise.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

state="$(hook_active_state)" || exit 0
blob="$(cat "$state" 2>/dev/null)"

missing=""
for g in $SW_GATE_ORDER; do
  [ -z "$(json_str "gate_${g}" "$blob")" ] && missing="$missing $g"
done
# All required gates recorded: nothing to block.
[ -n "$missing" ] || exit 0

flag="$(hook_state_dir)/completion-check.warned"
if [ -f "$flag" ]; then
  # Already warned once this session; allow the stop.
  exit 0
fi
mkdir -p "$(hook_state_dir)" 2>/dev/null || true
: >"$flag" 2>/dev/null || true
hook_block "Blocked (once): review gates still open:${missing}. Finish them or stop again to override."
