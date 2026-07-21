#!/usr/bin/env bash
# PreCompact: before the context window is compacted, refresh a small durable
# memory snapshot so the run can resume with its bearings. Capped at 200 lines.
# Inert when no run is active. Always exit 0 (never blocks a compaction).
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

state="$(hook_active_state)" || exit 0
run_dir="$(dirname "$state")"
mem="$run_dir/MEMORY.md"
ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
blob="$(cat "$state" 2>/dev/null)"

{
  echo "# Run memory snapshot"
  echo
  echo "Refreshed at $ts (PreCompact). This is durable run state; read it to resume."
  echo
  echo "## Run"
  echo "- run: $(json_str run "$blob")"
  echo "- phase: $(json_str phase "$blob")"
  echo
  echo "## Gate verdicts"
  for g in $SW_GATE_ORDER; do
    v="$(json_str "gate_${g}" "$blob")"
    echo "- $g: ${v:-pending}"
  done
  echo
  echo "## Open plan items"
  echo "- count: $(json_num plan_open "$blob")"
  echo
  echo "## State file"
  echo '```json'
  printf '%s\n' "$blob"
  echo '```'
} | head -n 200 >"$mem" 2>/dev/null || true

echo "save-session: snapshot refreshed at $mem"
exit 0
