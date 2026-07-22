#!/usr/bin/env bash
# Fast gate. Always runs the game-free tier and the Rust checks; when the game
# DLLs are present locally (lib/Assembly-CSharp.dll), it also builds the full
# solution and the game-coupled surface tests. Exits non-zero on any failure.
#
# CI runs the same underlying commands, so a green check here means the same
# thing as a green pipeline.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "==> instruction-layer conventions"
bash scripts/check-conventions.sh

echo "==> leak guard"
bash scripts/check-no-leak.sh

echo "==> guard: no game IP tracked"
bash scripts/guard-no-game-ip.sh

echo "==> governance hook golden-fixture suite"
bash scripts/test-hooks.sh

echo "==> build game-free solution filter (Release)"
dotnet build SailwindOnline.CI.slnf -c Release

echo "==> game-free dotnet tests"
dotnet test tests/Sailwind.Architecture.Tests -c Release
dotnet test tests/Sailwind.Contracts.Tests -c Release
dotnet test tests/Sailwind.Online.Net.Tests -c Release
dotnet test tests/Sailwind.Online.Sync.Tests -c Release
dotnet test tests/Sailwind.Api.SurfaceManifest.Tests -c Release
dotnet test tests/Sailwind.Templates.Tests -c Release

echo "==> cargo fmt (check)"
cargo fmt --all --check --manifest-path server/Cargo.toml

echo "==> cargo clippy (-D warnings)"
cargo clippy --workspace --all-targets --manifest-path server/Cargo.toml -- -D warnings

echo "==> cargo test"
cargo test --workspace --manifest-path server/Cargo.toml

echo "==> coverage (game-free C# + Rust, with thresholds)"
bash scripts/coverage.sh

if [ -f lib/Assembly-CSharp.dll ]; then
  echo "==> lib/ present: full solution build (Release)"
  dotnet build SailwindOnline.sln -c Release
  echo "==> game-coupled surface tests"
  dotnet test tests/Sailwind.Api.SurfaceTests -c Release
else
  echo "==> lib/Assembly-CSharp.dll absent: skipping game-coupled tier (runs locally / self-hosted)."
fi

echo "check: all gates passed."
