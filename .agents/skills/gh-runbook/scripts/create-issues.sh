#!/usr/bin/env bash
# create-issues.sh - turn a child-issue manifest into GitHub issues.
#
# DRY-RUN BY DEFAULT: it prints the gh commands it would run and creates
# nothing. Pass --apply to actually create the issues via gh. This split exists
# so a manifest can be reviewed before anything hits the board.
#
# Usage:
#   bash create-issues.sh                       # dry-run against the example manifest
#   bash create-issues.sh --manifest issues.md  # dry-run against your manifest
#   bash create-issues.sh --manifest issues.md --apply   # create for real
#
# Manifest format: blocks separated by a line reading exactly "=== issue ===".
# Each block has leading "key: value" lines then a "body:" line; everything
# after "body:" until the next block or EOF is the issue body. Recognised keys:
#   title, labels (comma-separated), milestone, blocked-by (comma-separated #N).
# blocked-by values are appended to the body under a "Blocked by" line, because
# GitHub has no native blocked-by field and the /work skill reads that text.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$script_dir/../references/example-manifest.md"
apply=0

while [ $# -gt 0 ]; do
  case "$1" in
    --manifest) manifest="$2"; shift 2 ;;
    --apply) apply=1; shift ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "create-issues: unknown argument '$1'" >&2; exit 2 ;;
  esac
done

if [ ! -f "$manifest" ]; then
  echo "create-issues: manifest not found: $manifest" >&2
  exit 1
fi

if [ "$apply" -eq 1 ]; then
  if ! command -v gh >/dev/null 2>&1; then
    echo "create-issues: --apply needs the gh CLI on PATH." >&2
    exit 1
  fi
  echo "create-issues: APPLY mode - issues will be created."
else
  echo "create-issues: DRY-RUN - no issue will be created. Pass --apply to create."
fi

title=""; labels=""; milestone=""; blocked=""; body=""; in_body=0; count=0

flush() {
  [ -n "$title" ] || return 0
  count=$((count + 1))

  local full_body="$body"
  case "$blocked" in
    ''|None|none|NONE) : ;;  # no real blocker; the /work skill treats absence and "None" alike
    *) full_body="${full_body}"$'\n\n'"## Blocked by"$'\n'"Blocked by ${blocked}" ;;
  esac

  local -a args=(issue create --title "$title" --body-file -)
  local IFS=,
  local l
  for l in $labels; do
    l="$(echo "$l" | sed 's/^ *//;s/ *$//')"
    [ -n "$l" ] && args+=(--label "$l")
  done
  unset IFS
  [ -n "$milestone" ] && args+=(--milestone "$milestone")

  if [ "$apply" -eq 1 ]; then
    printf '%s' "$full_body" | gh "${args[@]}"
  else
    echo "---"
    echo "would run: gh ${args[*]}"
    echo "  body:"
    printf '%s\n' "$full_body" | sed 's/^/    /'
  fi

  title=""; labels=""; milestone=""; blocked=""; body=""; in_body=0
}

while IFS= read -r line || [ -n "$line" ]; do
  if [ "$line" = "=== issue ===" ]; then
    flush
    continue
  fi
  if [ "$in_body" -eq 1 ]; then
    body="${body}${line}"$'\n'
    continue
  fi
  case "$line" in
    title:*)      title="$(echo "${line#title:}" | sed 's/^ *//;s/ *$//')" ;;
    labels:*)     labels="$(echo "${line#labels:}" | sed 's/^ *//;s/ *$//')" ;;
    milestone:*)  milestone="$(echo "${line#milestone:}" | sed 's/^ *//;s/ *$//')" ;;
    blocked-by:*) blocked="$(echo "${line#blocked-by:}" | sed 's/^ *//;s/ *$//')" ;;
    body:*)       in_body=1 ;;
    *)            : ;;  # ignore blank and stray lines outside a body
  esac
done < "$manifest"
flush

echo "create-issues: processed $count issue(s) from $manifest."
if [ "$apply" -eq 0 ]; then
  echo "create-issues: dry-run complete; nothing was created."
fi
