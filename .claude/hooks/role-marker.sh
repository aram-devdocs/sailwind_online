#!/usr/bin/env bash
# SessionStart: record this session's role into .claude/state/session.json.
# Default role is "solo"; a session is "orchestrator" only while a /work run is
# active, because only then may the delegation rules apply. Always exit 0.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

state_dir="$(hook_state_dir)"
mkdir -p "$state_dir" 2>/dev/null || true

role="solo"
if hook_active_state >/dev/null 2>&1; then
  role="orchestrator"
fi

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
printf '{\n  "role": "%s",\n  "started": "%s"\n}\n' "$role" "$ts" \
  >"$(hook_session_file)" 2>/dev/null || true

echo "session role: $role"
exit 0
