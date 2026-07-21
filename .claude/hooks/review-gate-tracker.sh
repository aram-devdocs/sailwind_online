#!/usr/bin/env bash
# PostToolUse (Agent|Task): after a gate subagent finishes, record its verdict
# into the active run state as gate_<name>. Bookkeeping only; always exit 0.
# Inert when no run is active.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

state="$(hook_active_state)" || exit 0

hook_read_payload

target="$(hook_field '.tool_input.gate')"
[ -n "$target" ] || target="$(hook_field '.tool_input.subagent_type')"
[ -n "$target" ] || target="$(hook_field '.subagent_type')"

gate=""
for g in $SW_GATE_ORDER; do
  case "$target" in *"$g"*) gate="$g"; break ;; esac
done
[ -n "$gate" ] || exit 0

# Pull the verdict from the subagent response text.
resp="$(hook_field '.tool_response')"
[ -n "$resp" ] || resp="$(hook_field '.output')"
[ -n "$resp" ] || resp="$HOOK_PAYLOAD"
verdict="$(printf '%s\n' "$resp" |
  grep -Eom1 -- '(APPROVE|REQUEST-CHANGES|REJECT)' | head -n1)"
[ -n "$verdict" ] || exit 0

ts="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"
blob="$(cat "$state" 2>/dev/null)"
key="gate_${gate}"
if printf '%s' "$blob" | grep -Eq -- "\"${key}\""; then
  updated="$(printf '%s' "$blob" |
    sed -E "s/\"${key}\"[[:space:]]*:[[:space:]]*\"[^\"]*\"/\"${key}\": \"${verdict}\"/")"
else
  # Insert the new key after the opening brace.
  updated="$(printf '%s' "$blob" |
    sed -E "0,/\\{/s/\\{/{\n  \"${key}\": \"${verdict}\",/")"
fi
printf '%s\n' "$updated" >"$state" 2>/dev/null || true
echo "review-gate-tracker: recorded $gate=$verdict at $ts"
exit 0
