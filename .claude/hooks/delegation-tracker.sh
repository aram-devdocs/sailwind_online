#!/usr/bin/env bash
# PostToolUse (Agent|Task): light bookkeeping of subagent dispatches during an
# active run. Never blocks; always exit 0. Inert when no run is active.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

state="$(hook_active_state)" || exit 0

hook_read_payload
sub="$(hook_field '.tool_input.subagent_type')"
[ -n "$sub" ] || sub="$(hook_field '.subagent_type')"
[ -n "$sub" ] || sub="subagent"

run_dir="$(dirname "$state")"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
printf '%s dispatch: %s\n' "$ts" "$sub" >>"$run_dir/dispatch.log" 2>/dev/null || true
exit 0
