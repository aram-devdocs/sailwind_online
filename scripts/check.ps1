# Fast gate (Windows mirror of check.sh). Always runs the game-free tier and the
# Rust checks; when lib/Assembly-CSharp.dll is present it also builds the full
# solution and the game-coupled surface tests. Exits non-zero on any failure.
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

function Invoke-Step {
    param([string]$Name, [scriptblock]$Body)
    Write-Host "==> $Name"
    & $Body
    if ($LASTEXITCODE -ne 0) {
        throw "step failed: $Name (exit $LASTEXITCODE)"
    }
}

Invoke-Step "instruction-layer conventions" { bash scripts/check-conventions.sh }
Invoke-Step "leak guard" { bash scripts/check-no-leak.sh }
Invoke-Step "guard: no game IP tracked" { powershell -NoProfile -ExecutionPolicy Bypass -File scripts/guard-no-game-ip.ps1 }
Invoke-Step "build game-free solution filter (Release)" { dotnet build SailwindOnline.CI.slnf -c Release }
Invoke-Step "game-free dotnet tests (architecture)" { dotnet test tests/Sailwind.Architecture.Tests -c Release }
Invoke-Step "game-free dotnet tests (contracts)" { dotnet test tests/Sailwind.Contracts.Tests -c Release }
Invoke-Step "game-free dotnet tests (net)" { dotnet test tests/Sailwind.Online.Net.Tests -c Release }
Invoke-Step "game-free dotnet tests (sync)" { dotnet test tests/Sailwind.Online.Sync.Tests -c Release }
Invoke-Step "cargo fmt (check)" { cargo fmt --all --check --manifest-path server/Cargo.toml }
Invoke-Step "cargo clippy (-D warnings)" { cargo clippy --workspace --all-targets --manifest-path server/Cargo.toml -- -D warnings }
Invoke-Step "cargo test" { cargo test --workspace --manifest-path server/Cargo.toml }

if (Test-Path 'lib/Assembly-CSharp.dll') {
    Invoke-Step "lib/ present: full solution build (Release)" { dotnet build SailwindOnline.sln -c Release }
    Invoke-Step "game-coupled surface tests" { dotnet test tests/Sailwind.Api.SurfaceTests -c Release }
} else {
    Write-Host "==> lib/Assembly-CSharp.dll absent: skipping game-coupled tier (runs locally / self-hosted)."
}

Write-Host "check: all gates passed."
