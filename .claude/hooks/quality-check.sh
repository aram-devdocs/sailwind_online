#!/usr/bin/env bash
# PostToolUse (Write|Edit|MultiEdit): format the edited file in place and print a
# short lint note. This never blocks a session (the gate `make validate` is the
# real enforcer); it always exits 0 so a missing formatter or a format failure
# is a note, not an interruption.
set -uo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/_lib.sh"

hook_read_payload
path="$(hook_field '.tool_input.file_path')"
[ -n "$path" ] || exit 0
[ -f "$path" ] || exit 0

case "$path" in
*.cs)
  if command -v dotnet >/dev/null 2>&1; then
    dotnet format whitespace --include "$path" >/dev/null 2>&1 &&
      echo "quality-check: dotnet format applied to $path" ||
      echo "quality-check: dotnet format skipped (note: run make validate before push)"
  fi
  ;;
*.rs)
  if command -v cargo >/dev/null 2>&1; then
    cargo fmt -- "$path" >/dev/null 2>&1 &&
      echo "quality-check: cargo fmt applied to $path" ||
      echo "quality-check: cargo fmt skipped (note: run make validate before push)"
  fi
  ;;
*.sh)
  if command -v shfmt >/dev/null 2>&1; then
    shfmt -w "$path" >/dev/null 2>&1 &&
      echo "quality-check: shfmt applied to $path"
  else
    echo "quality-check: shfmt not installed; skipping shell format (non-blocking)"
  fi
  ;;
esac

exit 0
