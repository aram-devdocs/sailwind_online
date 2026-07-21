#!/usr/bin/env bash
# PreToolUse (Bash): block destructive or gate-bypassing shell commands. This
# supersedes the retired block-no-verify.sh. Exit 2 to block, 0 otherwise.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

hook_read_payload
cmd="$(hook_field '.tool_input.command')"
[ -n "$cmd" ] || exit 0

# 1. --no-verify bypasses the git hooks; a bypassed gate is no gate.
if printf '%s' "$cmd" | grep -Eq -- '--no-verify'; then
  hook_block "Blocked: --no-verify bypasses the git hooks and is forbidden."
fi

# 2. Rerouting core.hooksPath disables the versioned hooks.
if printf '%s' "$cmd" | grep -Eq -- 'core\.hooksPath[[:space:]]*='; then
  hook_block "Blocked: overriding core.hooksPath disables the versioned hooks."
fi

# 3. Force-pushing a protected branch (main/dev) rewrites shared history.
if printf '%s' "$cmd" | grep -Eq -- 'git[[:space:]]+push' &&
  printf '%s' "$cmd" | grep -Eq -- '(--force([[:space:]]|=|$)|--force-with-lease|(^|[[:space:]])-f([[:space:]]|$))' &&
  printf '%s' "$cmd" | grep -Eq -- '(^|[[:space:]:/])(main|dev)([[:space:]]|$|:)'; then
  hook_block "Blocked: force-pushing a protected branch (main/dev) is forbidden."
fi

# 4. git reset --hard on a dirty tree discards uncommitted work.
if printf '%s' "$cmd" | grep -Eq -- 'git[[:space:]]+reset[[:space:]]+(--hard|.*[[:space:]]--hard)'; then
  dirty=""
  if [ -n "${SW_TREE_DIRTY:-}" ]; then
    [ "$SW_TREE_DIRTY" = "1" ] && dirty="yes"
  elif [ -n "$(git status --porcelain 2>/dev/null)" ]; then
    dirty="yes"
  fi
  if [ -n "$dirty" ]; then
    hook_block "Blocked: git reset --hard on a dirty tree discards uncommitted work; commit or stash first."
  fi
fi

# 5. rm -rf (recursive AND force, flags in any order) outside a scratch area.
if printf '%s' "$cmd" | grep -Eq -- '(^|[[:space:]])rm[[:space:]]' &&
  printf '%s' "$cmd" | grep -Eq -- 'rm[[:space:]].*-[A-Za-z]*r' &&
  printf '%s' "$cmd" | grep -Eq -- 'rm[[:space:]].*-[A-Za-z]*f'; then
  if ! printf '%s' "$cmd" |
    grep -Eiq -- '(scratch|/tmp/|[/ ]temp[/ ]|[/ ]tmp[/ ]|target/|/bin/|/obj/|node_modules|\.cache|AppData[/\\]Local[/\\]Temp)'; then
    hook_block "Blocked: rm -rf outside a scratch/temp/build path; scope the deletion or use a temp dir."
  fi
fi

exit 0
