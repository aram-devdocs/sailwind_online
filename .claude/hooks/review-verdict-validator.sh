#!/usr/bin/env bash
# SubagentStop (reviewer/auditor agents): a review with no verdict is no pass.
# If the subagent output lacks a greppable verdict line (first token APPROVE,
# REQUEST-CHANGES, or REJECT on its own line), block (exit 2) and demand one.
# Inert (exit 0) for non-reviewer subagents.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

hook_read_payload

role="$(hook_field '.subagent_type')"
[ -n "$role" ] || role="$(hook_field '.agent_type')"
[ -n "$role" ] || role="$(hook_field '.agent')"

is_reviewer=""
case "$role" in
*review* | *audit*) is_reviewer="yes" ;;
esac
# Non-reviewer subagents are out of scope for the verdict rule.
[ -n "$is_reviewer" ] || exit 0

# Gather the reviewer's output text from the likely fields, plus the transcript
# tail when a path is provided.
output="$(hook_field '.output')"
[ -n "$output" ] || output="$(hook_field '.last_message')"
[ -n "$output" ] || output="$(hook_field '.tool_response')"
transcript="$(hook_field '.transcript_path')"
if [ -n "$transcript" ] && [ -f "$transcript" ]; then
  output="$output
$(tail -c 20000 "$transcript" 2>/dev/null)"
fi
# Last resort: scan the whole payload.
[ -n "$output" ] || output="$HOOK_PAYLOAD"
# Decode JSON \n escapes so the verdict lands on its own line even without jq.
output="$(printf '%s' "$output" | sed 's/\\n/\n/g')"

if printf '%s\n' "$output" | grep -Eq -- '^(APPROVE|REQUEST-CHANGES|REJECT)([[:space:]]|$)'; then
  exit 0
fi

hook_block "Blocked: review produced no verdict. End with one greppable line whose first token is APPROVE, REQUEST-CHANGES, or REJECT. No verdict means no pass."
