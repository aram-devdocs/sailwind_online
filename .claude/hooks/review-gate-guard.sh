#!/usr/bin/env bash
# PreToolUse (Agent|Task): keep the review gates in their fixed order
# (spec -> quality -> architecture -> security). Inert unless a /work run is
# active. When active, block launching a gate before every earlier gate has a
# recorded verdict. Exit 2 to block, 0 otherwise.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

# Inert by default: no active run means no gate ordering to enforce.
state="$(hook_active_state)" || exit 0

hook_read_payload

# Which gate is this launch for? Prefer an explicit field, then the subagent
# type, then a scan of the payload for a single gate keyword.
target="$(hook_field '.tool_input.gate')"
[ -n "$target" ] || target="$(hook_field '.tool_input.subagent_type')"
[ -n "$target" ] || target="$(hook_field '.subagent_type')"

gate=""
for g in $SW_GATE_ORDER; do
  case "$target" in *"$g"*) gate="$g"; break ;; esac
done
if [ -z "$gate" ]; then
  for g in $SW_GATE_ORDER; do
    if printf '%s' "$HOOK_PAYLOAD" | grep -Eiq -- "\\b${g}\\b"; then
      gate="$g"
      break
    fi
  done
fi
# Not a review-gate launch: nothing to order.
[ -n "$gate" ] || exit 0

blob="$(cat "$state" 2>/dev/null)"
for g in $SW_GATE_ORDER; do
  [ "$g" = "$gate" ] && break
  verdict="$(json_str "gate_${g}" "$blob")"
  if [ -z "$verdict" ]; then
    hook_block "Blocked: cannot start the '$gate' gate before '$g' has a recorded verdict. Run the gates in order: spec -> quality -> architecture -> security."
  fi
done

exit 0
