#Requires -Version 5.1
<#
.SYNOPSIS
    Downloads the pinned flatc 25.2.10 Windows binary into .tools/flatc and
    verifies it against contracts/flatc.sha256. Idempotent: no-ops when a
    matching flatc.exe is already present.
.NOTES
    Parity mirror: scripts/get-flatc.sh (Linux).
#>
[CmdletBinding()]
param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# Repo root = parent of this script's directory.
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RepoRoot  = Split-Path -Parent $ScriptDir

$Version   = '25.2.10'
$Asset     = 'Windows.flatc.binary.zip'
$Url       = "https://github.com/google/flatbuffers/releases/download/v$Version/$Asset"
$ToolsDir  = Join-Path $RepoRoot '.tools/flatc'
$ExePath   = Join-Path $ToolsDir 'flatc.exe'
$ZipPath   = Join-Path $ToolsDir $Asset
$Sha256File = Join-Path $RepoRoot 'contracts/flatc.sha256'

function Get-ExpectedHash {
    param([string]$AssetName)
    if (-not (Test-Path $Sha256File)) {
        throw "Checksum file not found: $Sha256File"
    }
    foreach ($line in Get-Content $Sha256File) {
        $trimmed = $line.Trim()
        if ([string]::IsNullOrWhiteSpace($trimmed)) { continue }
        # Format: "<sha256>  <filename>"
        $parts = $trimmed -split '\s+', 2
        if ($parts.Count -eq 2 -and $parts[1].Trim() -eq $AssetName) {
            return $parts[0].ToLowerInvariant()
        }
    }
    throw "No checksum entry for '$AssetName' in $Sha256File"
}

# Idempotency: if flatc.exe exists and reports the pinned version, we're done.
if ((Test-Path $ExePath) -and (-not $Force)) {
    try {
        $reported = (& $ExePath --version) 2>$null
        if ($reported -match [regex]::Escape($Version)) {
            Write-Host "flatc $Version already present at $ExePath"
            exit 0
        }
    } catch {
        # Fall through and re-download.
    }
}

New-Item -ItemType Directory -Force -Path $ToolsDir | Out-Null

$expected = Get-ExpectedHash -AssetName $Asset

Write-Host "Downloading $Url"
$oldProgress = $ProgressPreference
$ProgressPreference = 'SilentlyContinue'
try {
    Invoke-WebRequest -Uri $Url -OutFile $ZipPath -UseBasicParsing
} finally {
    $ProgressPreference = $oldProgress
}

$actual = (Get-FileHash -Path $ZipPath -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actual -ne $expected) {
    Remove-Item -Force $ZipPath -ErrorAction SilentlyContinue
    throw "SHA-256 mismatch for ${Asset}: expected $expected, got $actual"
}
Write-Host "SHA-256 verified: $actual"

if (Test-Path $ExePath) { Remove-Item -Force $ExePath }
Expand-Archive -Path $ZipPath -DestinationPath $ToolsDir -Force

if (-not (Test-Path $ExePath)) {
    throw "flatc.exe not found after extracting $Asset"
}

$reported = (& $ExePath --version) 2>$null
Write-Host "Installed: $reported"
Write-Host "flatc ready at $ExePath"
