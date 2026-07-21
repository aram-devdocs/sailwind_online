#!/usr/bin/env bash
# PreToolUse guard for Bash commands. Blocks any attempt to bypass git hooks,
# because a bypassed gate is a gate that does not exist (see AGENTS.md).
# Reads the Claude Code tool-call JSON on stdin, extracts the command string,
# and exits 2 (block) when the command disables or reroutes the hook path.
set -euo pipefail

payload="$(cat)"

# Pull the command field out of the JSON. Prefer jq; fall back to a grep of the
# raw payload so the guard still fires if jq is unavailable.
if command -v jq >/dev/null 2>&1; then
  cmd="$(printf '%s' "$payload" | jq -r '.tool_input.command // ""')"
else
  cmd="$payload"
fi

if printf '%s' "$cmd" | grep -Eq -- '--no-verify'; then
  echo "Blocked: --no-verify bypasses git hooks and is forbidden (AGENTS.md)." >&2
  exit 2
fi

if printf '%s' "$cmd" | grep -Eq -- '(-c[[:space:]]+core\.hooksPath=|core\.hooksPath=/dev/null|git config[[:space:]]+core\.hooksPath)'; then
  echo "Blocked: rerouting core.hooksPath disables the versioned hooks and is forbidden (AGENTS.md)." >&2
  exit 2
fi

exit 0
