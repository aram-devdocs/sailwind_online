#!/usr/bin/env bash
# SubagentStart: for reviewer or auditor subagents, print the challenge-protocol
# self-verification reminder so the review starts adversarial. Inert (silent
# pass) for every other subagent role. Always exit 0.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

hook_read_payload

# Role can arrive under several keys depending on the harness; also fall back to
# scanning the payload text for a reviewer/auditor marker.
role="$(hook_field '.subagent_type')"
[ -n "$role" ] || role="$(hook_field '.agent_type')"
[ -n "$role" ] || role="$(hook_field '.agent')"

is_reviewer=""
case "$role" in
*review* | *audit*) is_reviewer="yes" ;;
esac
if [ -z "$is_reviewer" ] &&
  printf '%s' "$HOOK_PAYLOAD" | grep -Eiq -- 'review|audit'; then
  is_reviewer="yes"
fi

[ -n "$is_reviewer" ] || exit 0

proto="$(hook_repo_root)/.agents/rules/challenge-protocol.md"
echo "Challenge protocol is in force for this review:"
if [ -f "$proto" ]; then
  echo "Read $proto and apply it to every finding."
fi
echo "Cite path:line, re-read before citing, run the check before claiming it"
echo "passes, and ask what input would make each finding false. End with a"
echo "single greppable verdict line: APPROVE, REQUEST-CHANGES, or REJECT."
exit 0
