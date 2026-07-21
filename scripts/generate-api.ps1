<#
.SYNOPSIS
  Run Sailwind.ApiGen against lib/Assembly-CSharp.dll: verify the seed manifest and
  regenerate GameRef.g.cs, api-surface.json (+ .sha256), SurfaceManifest.g.cs and
  SurfaceContract.g.cs. Requires lib/ (run scripts/setup-game.ps1 first).
#>
[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$RepoRoot = Split-Path $PSScriptRoot -Parent
$Assembly = Join-Path $RepoRoot "lib\Assembly-CSharp.dll"

if (-not (Test-Path $Assembly)) {
    throw "lib\Assembly-CSharp.dll not found. Run scripts/setup-game.ps1 first."
}

Push-Location $RepoRoot
try {
    dotnet run --project (Join-Path $RepoRoot "tools\Sailwind.ApiGen") -c Release
    if ($LASTEXITCODE -ne 0) { throw "ApiGen failed (exit $LASTEXITCODE)." }
}
finally {
    Pop-Location
}
