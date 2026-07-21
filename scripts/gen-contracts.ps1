#Requires -Version 5.1
<#
.SYNOPSIS
    Regenerates the committed FlatBuffers bindings (C# + Rust) from
    contracts/fbs/envelope.fbs using the pinned flatc in .tools/flatc.
    Reproduces the committed output byte-for-byte; CI diffs afterward.
.NOTES
    Parity mirror: scripts/gen-contracts.sh (Linux).
    Ensure flatc is present first: scripts/get-flatc.ps1
#>
[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot  = Split-Path -Parent $ScriptDir

$Flatc   = Join-Path $RepoRoot '.tools/flatc/flatc.exe'
$Schema  = Join-Path $RepoRoot 'contracts/fbs/envelope.fbs'
$CsOut   = Join-Path $RepoRoot 'contracts/cs/Sailwind.Contracts/Generated'
$RustOut = Join-Path $RepoRoot 'server/crates/sw-contracts/src/generated'

if (-not (Test-Path $Flatc)) {
    throw "flatc not found at $Flatc. Run scripts/get-flatc.ps1 first."
}
if (-not (Test-Path $Schema)) {
    throw "Schema not found: $Schema"
}

New-Item -ItemType Directory -Force -Path $CsOut   | Out-Null
New-Item -ItemType Directory -Force -Path $RustOut | Out-Null

Write-Host "flatc: $((& $Flatc --version))"

Write-Host "Generating C# -> $CsOut"
& $Flatc --csharp --gen-all -o $CsOut $Schema
if ($LASTEXITCODE -ne 0) { throw "flatc --csharp failed (exit $LASTEXITCODE)" }

Write-Host "Generating Rust -> $RustOut"
& $Flatc --rust --gen-all -o $RustOut $Schema
if ($LASTEXITCODE -ne 0) { throw "flatc --rust failed (exit $LASTEXITCODE)" }

Write-Host "Contracts regenerated."
