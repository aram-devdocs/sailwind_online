#!/usr/bin/env bash
# Convention guard for the DRY instruction layer and the coverage gate wiring.
# Validates via the git index mode (not the filesystem) for the symlink checks,
# so it is correct on Windows checkouts where symlinks materialize as text
# stand-ins as well as on symlink-capable checkouts.
#
# Enforces:
#   1. Every scoped AGENTS.md is under its line budget (root 100, scoped 60).
#   2. Each AGENTS.md has a sibling CLAUDE.md recorded as a symlink (git mode
#      120000) whose target is "AGENTS.md".
#   3. .claude/rules and .claude/skills are recorded as symlinks into ../.agents/.
#   4. Coverage collection and its fail-under threshold are wired into the
#      required gate: coverlet.collector is referenced centrally, the ci.yml
#      `coverage` job feeds the required `gate`, and both check scripts run
#      C# + Rust coverage with a `--fail-under-lines` threshold.
#   5. Coverage-set completeness: every game-free test project in
#      SailwindOnline.CI.slnf except Sailwind.Architecture.Tests is named in
#      scripts/coverage.sh, so a new game-free test project cannot silently drop
#      out of the C# coverage measurement (and thus its floor).
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

# 4: coverage gate wiring. Coverage regressions are caught only if collection
# and the fail-under threshold actually reach the required gate, so pin each
# seam. grep against the committed working tree (these are plain text files).
have() { grep -Eq "$1" "$2" 2>/dev/null; }

# coverlet.collector must be referenced centrally so every game-free test
# project emits Cobertura under `--collect:"XPlat Code Coverage"`.
have 'coverlet\.collector' Directory.Build.props \
  || err "Directory.Build.props must reference coverlet.collector so game-free tests emit coverage."

# The shared coverage script must collect C# (XPlat Code Coverage -> Cobertura)
# and Rust (cargo llvm-cov) line coverage and enforce a fail-under threshold.
have 'XPlat Code Coverage' scripts/coverage.sh \
  || err "scripts/coverage.sh must collect C# coverage via 'XPlat Code Coverage'."
have 'cargo llvm-cov' scripts/coverage.sh \
  || err "scripts/coverage.sh must collect Rust coverage via 'cargo llvm-cov'."
have 'fail-under-lines' scripts/coverage.sh \
  || err "scripts/coverage.sh must enforce a '--fail-under-lines' coverage threshold."

# The ci.yml `coverage` job must exist and feed the single required `gate`.
have '^  coverage:' .github/workflows/ci.yml \
  || err ".github/workflows/ci.yml must define a 'coverage' job."
have '^    needs: \[.*coverage.*\]' .github/workflows/ci.yml \
  || err ".github/workflows/ci.yml gate.needs must include 'coverage' so the threshold is enforced."

# Both check scripts must run the coverage gate so `make validate` mirrors CI.
for script in scripts/check.sh scripts/check.ps1; do
  have 'scripts/coverage\.sh' "$script" \
    || err "$script must run the coverage gate via scripts/coverage.sh."
done

# 5: coverage-set completeness. coverage.sh measures C# coverage over an explicit
# list of game-free UNIT-test projects; Sailwind.Architecture.Tests is excluded
# from that MEASUREMENT (its Cecil AssemblyScanner is incompatible with coverlet
# instrumentation on Linux) but still runs unfiltered in the `dotnet` gate. A new
# game-free test project could silently be added to SailwindOnline.CI.slnf but
# left out of coverage.sh, quietly shrinking what the floor protects. Assert every
# game-free test project in the CI filter, except Architecture.Tests, is named in
# coverage.sh so the coverage set cannot regress by omission.
slnf="SailwindOnline.CI.slnf"
while IFS= read -r testproj; do
  [ -n "$testproj" ] || continue
  [ "$testproj" = "Sailwind.Architecture.Tests" ] && continue
  have "$testproj" scripts/coverage.sh \
    || err "$testproj is a game-free test project in $slnf but is absent from scripts/coverage.sh's coverage set."
done < <(grep -oE 'Sailwind\.[A-Za-z.]+\.Tests\.csproj' "$slnf" | sed 's/\.csproj$//' | sort -u)

if [ "$fail" -ne 0 ]; then
  echo "check-conventions: FAILED." >&2
  exit 1
fi
echo "check-conventions: clean."
