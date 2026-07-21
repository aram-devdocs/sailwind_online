#!/usr/bin/env bash
# Shared helpers for the Claude Code governance hooks. Sourced, never executed.
#
# Contract every hook follows:
#   - Read the Claude Code tool payload from stdin as JSON. jq is preferred; a
#     raw grep fallback keeps the hooks working when jq is not installed.
#   - Resolve the repo root with `git rev-parse --show-toplevel`.
#   - Exit codes: 0 = pass, 2 = block with a one-line reason on stdout, any
#     other value = non-blocking log.
#
# Test seams (never set in normal sessions, only by scripts/test-hooks.sh):
#   SW_RUNS_DIR      run-state directory   (default <root>/.agents/runs)
#   SW_STATE_DIR     session-state dir     (default <root>/.claude/state)
#   SW_SESSION_FILE  session role file     (default <state>/session.json)
#   SW_TREE_DIRTY    override dirty-tree probe with 1/0 (default: real git)

# The four review gates in their fixed order (spec first, security last).
SW_GATE_ORDER="spec quality architecture security"

# Repo root. Falls back to this library's own location (<root>/.claude/hooks).
hook_repo_root() {
  local r
  if r="$(git rev-parse --show-toplevel 2>/dev/null)"; then
    printf '%s\n' "$r"
    return 0
  fi
  (cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
}

hook_runs_dir() { printf '%s\n' "${SW_RUNS_DIR:-$(hook_repo_root)/.agents/runs}"; }
hook_state_dir() { printf '%s\n' "${SW_STATE_DIR:-$(hook_repo_root)/.claude/state}"; }
hook_session_file() { printf '%s\n' "${SW_SESSION_FILE:-$(hook_state_dir)/session.json}"; }

# Read all of stdin into HOOK_PAYLOAD. Call once at the top of a hook.
hook_read_payload() { HOOK_PAYLOAD="$(cat)"; }

# First "key":"value" string for a leaf key inside a JSON blob argument.
json_str() {
  local key="$1" blob="$2"
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "$blob" | jq -r ".${key} // \"\"" 2>/dev/null
    return 0
  fi
  printf '%s' "$blob" |
    grep -oE "\"${key}\"[[:space:]]*:[[:space:]]*\"[^\"]*\"" |
    head -n1 |
    sed -E "s/.*\"${key}\"[[:space:]]*:[[:space:]]*\"([^\"]*)\".*/\1/"
}

# First numeric value for a leaf key inside a JSON blob argument.
json_num() {
  local key="$1" blob="$2"
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "$blob" | jq -r ".${key} // 0" 2>/dev/null
    return 0
  fi
  printf '%s' "$blob" |
    grep -oE "\"${key}\"[[:space:]]*:[[:space:]]*[0-9]+" |
    head -n1 |
    grep -oE '[0-9]+$'
}

# Field from HOOK_PAYLOAD by dotted jq path (fallback uses the last segment).
hook_field() {
  local jqpath="$1"
  if command -v jq >/dev/null 2>&1; then
    printf '%s' "${HOOK_PAYLOAD:-}" | jq -r "${jqpath} // \"\"" 2>/dev/null
    return 0
  fi
  json_str "${jqpath##*.}" "${HOOK_PAYLOAD:-}"
}

# Path to the active run's state.json, or nothing. Return 0 when a run is active.
# A run is active when .agents/runs/active names a run whose state.json exists,
# or when any run's state.json has a phase field that is not "done".
hook_active_state() {
  local runs
  runs="$(hook_runs_dir)"
  [ -d "$runs" ] || return 1
  if [ -f "$runs/active" ]; then
    local named
    named="$(head -n1 "$runs/active" 2>/dev/null | tr -d '[:space:]\r')"
    if [ -n "$named" ] && [ -f "$runs/$named/state.json" ]; then
      printf '%s\n' "$runs/$named/state.json"
      return 0
    fi
  fi
  local sj phase
  for sj in "$runs"/*/state.json; do
    [ -f "$sj" ] || continue
    phase="$(json_str phase "$(cat "$sj" 2>/dev/null)")"
    if [ -n "$phase" ] && [ "$phase" != "done" ]; then
      printf '%s\n' "$sj"
      return 0
    fi
  done
  return 1
}

# Session role recorded by role-marker.sh. Defaults to "solo".
hook_session_role() {
  local f
  f="$(hook_session_file)"
  if [ -f "$f" ]; then
    local r
    r="$(json_str role "$(cat "$f" 2>/dev/null)")"
    printf '%s\n' "${r:-solo}"
  else
    printf 'solo\n'
  fi
}

# Block: print the one-line reason on stdout and exit 2.
hook_block() {
  printf '%s\n' "$*"
  exit 2
}
