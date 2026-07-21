#!/usr/bin/env bash
# Convention guard for the DRY instruction layer. Validates via the git index mode
# (not the filesystem), so it is correct on Windows checkouts where symlinks
# materialize as text stand-ins as well as on symlink-capable checkouts.
#
# Enforces:
#   1. Every scoped AGENTS.md is under its line budget (root 100, scoped 60).
#   2. Each AGENTS.md has a sibling CLAUDE.md recorded as a symlink (git mode
#      120000) whose target is "AGENTS.md".
#   3. .claude/rules and .claude/skills are recorded as symlinks into ../.agents/.
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"

fail=0
err() { echo "check-conventions: $*" >&2; fail=1; }

git_mode() { git ls-files -s -- "$1" 2>/dev/null | awk '{print $1}'; }
blob_of()  { git ls-files -s -- "$1" 2>/dev/null | awk '{print $2}'; }

# 1 + 2: every tracked AGENTS.md
while IFS= read -r agents; do
  [ -n "$agents" ] || continue
  dir="$(dirname "$agents")"
  budget=60; [ "$dir" = "." ] && budget=100
  lines="$(git show ":$agents" | wc -l)"
  [ "$lines" -le "$budget" ] || err "$agents is $lines lines (budget $budget)."

  claude="$agents"; claude="${claude%AGENTS.md}CLAUDE.md"
  mode="$(git_mode "$claude")"
  if [ "$mode" != "120000" ]; then
    err "$claude must be a symlink (git mode 120000); found '${mode:-missing}'."
  else
    target="$(git cat-file blob "$(blob_of "$claude")")"
    [ "$target" = "AGENTS.md" ] || err "$claude points at '$target', expected 'AGENTS.md'."
  fi
done < <(git ls-files -- 'AGENTS.md' '**/AGENTS.md')

# 3: .claude/{rules,skills} symlinks into ../.agents/
for pair in ".claude/rules:../.agents/rules" ".claude/skills:../.agents/skills"; do
  link="${pair%%:*}"; want="${pair##*:}"
  mode="$(git_mode "$link")"
  if [ "$mode" != "120000" ]; then
    err "$link must be a symlink (git mode 120000); found '${mode:-missing}'."
  else
    target="$(git cat-file blob "$(blob_of "$link")")"
    [ "$target" = "$want" ] || err "$link points at '$target', expected '$want'."
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "check-conventions: FAILED." >&2
  exit 1
fi
echo "check-conventions: clean."
