#!/usr/bin/env bash
# PreToolUse (Write|Edit): during an active /work run the orchestrator must not
# write code directly; it dispatches the 01-implementer subagent. Inert unless a
# run is active AND this session's role is orchestrator. Exit 2 to block, else 0.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

# Inert by default: manual (solo) sessions edit freely.
hook_active_state >/dev/null 2>&1 || exit 0
[ "$(hook_session_role)" = "orchestrator" ] || exit 0

hook_read_payload
path="$(hook_field '.tool_input.file_path')"

hook_block "Blocked: the orchestrator does not write files directly during a run. Dispatch the 01-implementer subagent to edit ${path:-this file}."
