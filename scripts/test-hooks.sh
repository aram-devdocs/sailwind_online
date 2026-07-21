#!/usr/bin/env bash
# Golden-fixture runner for the Claude Code governance hooks.
#
# For every fixture .claude/hooks/fixtures/<hook>/<case>.expect<N>.json this feeds
# the JSON on stdin to <hook>.sh and asserts the hook exits with code <N>
# (0 = pass, 2 = block). Each case runs against a fresh, isolated world so the
# real .agents/runs and .claude/state are never touched:
#
#   <case>.world/   optional dir copied into a temp world; its runs/, state/, and
#                   session.json become SW_RUNS_DIR, SW_STATE_DIR, SW_SESSION_FILE.
#   <case>.env      optional KEY=VALUE lines exported before the hook runs.
#
# With no <case>.world, the temp world is empty, so a run is NOT active and the
# enforcing hooks are inert by default (their expect0 cases). Prints PASS/FAIL per
# case and exits 1 on any mismatch.
set -uo pipefail

root="$(git rev-parse --show-toplevel)"
hooks_dir="$root/.claude/hooks"
fixtures_dir="$hooks_dir/fixtures"

pass=0
fail=0

if [ ! -d "$fixtures_dir" ]; then
  echo "test-hooks: no fixtures directory at $fixtures_dir" >&2
  exit 1
fi

for fixture in $(find "$fixtures_dir" -type f -name '*.expect0.json' -o -type f -name '*.expect2.json' | sort); do
  hook_name="$(basename "$(dirname "$fixture")")"
  hook="$hooks_dir/$hook_name.sh"
  base="$(basename "$fixture")"
  case_name="${base%.expect*.json}"
  expected="${base##*.expect}"
  expected="${expected%.json}"

  if [ ! -f "$hook" ]; then
    echo "FAIL  $hook_name/$case_name  (no hook at $hook)"
    fail=$((fail + 1))
    continue
  fi

  world_src="$(dirname "$fixture")/$case_name.world"
  env_file="$(dirname "$fixture")/$case_name.env"

  tmp="$(mktemp -d)"
  mkdir -p "$tmp/runs" "$tmp/state"
  if [ -d "$world_src" ]; then
    cp -r "$world_src/." "$tmp/"
  fi

  (
    export SW_RUNS_DIR="$tmp/runs"
    export SW_STATE_DIR="$tmp/state"
    export SW_SESSION_FILE="$tmp/session.json"
    if [ -f "$env_file" ]; then
      set -a
      # shellcheck disable=SC1090
      . "$env_file"
      set +a
    fi
    bash "$hook" <"$fixture" >/dev/null 2>&1
    exit $?
  )
  actual=$?

  rm -rf "$tmp"

  if [ "$actual" = "$expected" ]; then
    echo "PASS  $hook_name/$case_name  (exit $actual)"
    pass=$((pass + 1))
  else
    echo "FAIL  $hook_name/$case_name  (expected $expected, got $actual)"
    fail=$((fail + 1))
  fi
done

echo
echo "test-hooks: $pass passed, $fail failed."
[ "$fail" -eq 0 ] || exit 1
