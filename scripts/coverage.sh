#!/usr/bin/env bash
# Coverage gate for the game-free tiers. Two languages, one floor each:
#   * C#   - the game-free solution filter (SailwindOnline.CI.slnf) via
#            coverlet.collector -> per-project Cobertura, merged by
#            ReportGenerator into a single line-coverage number.
#   * Rust - the server workspace via cargo llvm-cov.
# A tier whose line coverage drops below its floor exits non-zero, so a coverage
# regression fails the required gate.
#
# GAME-FREE: only SailwindOnline.CI.slnf is measured (the game-coupled
# Sailwind.Api.SurfaceTests is not in the filter) and lib/ is never touched.
#
# DETERMINISTIC: the floors sit a few points below the measured current coverage
# so the gate is green now and bites on a real regression. CI pins the tool
# versions (dotnet-reportgenerator-globaltool, cargo-llvm-cov) and the Rust
# llvm-tools-preview component.
#
# MISSING-TOOL GUARD: a contributor without cargo-llvm-cov or ReportGenerator can
# still run `make validate`; the relevant tier is SKIPPED with a notice. In CI
# (the CI env var is set) a missing tool is a hard error instead, so the gate is
# always enforced there and can never silently pass.
#
# Reports land under artifacts/coverage (gitignored) for the CI artifact upload.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Line-coverage floors. Measured 2026-07-22: C# 43.5% (merged, game-free tier),
# Rust 69.4% (server workspace). Floors sit below to catch regressions.
CS_MIN=40
RUST_MIN=60

out="artifacts/coverage"
rm -rf "$out"
mkdir -p "$out"

# dotnet global tools (ReportGenerator) install here on Linux and Windows alike.
export PATH="$PATH:$HOME/.dotnet/tools"

require_in_ci() {
  # $1 = tool description. In CI a missing coverage tool must fail the gate
  # rather than silently skip; locally it is a skip so `make validate` still runs.
  if [ -n "${CI:-}" ]; then
    echo "coverage: $1 is required in CI but is not installed." >&2
    exit 1
  fi
}

# --- C# (game-free tier) ---
if command -v reportgenerator >/dev/null 2>&1; then
  echo "==> C# coverage (game-free tier), floor ${CS_MIN}%"
  dotnet test SailwindOnline.CI.slnf -c Release \
    --collect:"XPlat Code Coverage" \
    --results-directory "$out/cs"
  reportgenerator \
    "-reports:$out/cs/**/coverage.cobertura.xml" \
    "-targetdir:$out/cs-report" \
    "-reporttypes:TextSummary;Cobertura;Html"
  rate="$(grep -oE 'line-rate="[0-9.]+"' "$out/cs-report/Cobertura.xml" | head -1 | grep -oE '[0-9.]+')"
  pct="$(awk -v r="$rate" 'BEGIN{printf "%.1f", r*100}')"
  echo "C# line coverage: ${pct}% (floor ${CS_MIN}%)"
  awk -v p="$pct" -v m="$CS_MIN" 'BEGIN{exit !(p+0 >= m+0)}' \
    || { echo "coverage: C# line coverage ${pct}% is below the ${CS_MIN}% floor." >&2; exit 1; }
else
  require_in_ci "dotnet-reportgenerator-globaltool (reportgenerator)"
  echo "==> reportgenerator absent: skipping C# coverage (install dotnet-reportgenerator-globaltool; CI enforces it)."
fi

# --- Rust (server workspace) ---
if cargo llvm-cov --version >/dev/null 2>&1; then
  echo "==> Rust coverage (server workspace), floor ${RUST_MIN}%"
  cargo llvm-cov --manifest-path server/Cargo.toml --workspace \
    --fail-under-lines "$RUST_MIN" \
    --cobertura --output-path "$out/rust.cobertura.xml"
else
  require_in_ci "cargo-llvm-cov"
  echo "==> cargo-llvm-cov absent: skipping Rust coverage (install cargo-llvm-cov + llvm-tools-preview; CI enforces it)."
fi

echo "coverage: gates passed."
