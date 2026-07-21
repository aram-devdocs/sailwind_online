<#
.SYNOPSIS
  Create a dedicated "SailwindOnline" Thunderstore Mod Manager profile for testing
  our plugins in isolation, so the user's Default profile (their own mod set) is
  never touched.

.DESCRIPTION
  Copies the BepInEx core + config and the Doorstop loader from an existing source
  profile (Default by default) into a fresh SailwindOnline profile, with an empty
  plugins folder. `make deploy` then places only our plugins there. Idempotent:
  re-running refreshes the loader/core without deleting the profile.
#>
[CmdletBinding()]
param(
    [string]$SourceProfile = "Default",
    [string]$Name = "SailwindOnline"
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$profiles = Join-Path $env:APPDATA "Thunderstore Mod Manager\DataFolder\Sailwind\profiles"
$src = Join-Path $profiles $SourceProfile
$dst = Join-Path $profiles $Name

if (-not (Test-Path $src)) { throw "Source profile not found: $src" }

Write-Host "Creating test profile: $dst"
New-Item -ItemType Directory -Force -Path (Join-Path $dst "BepInEx\plugins") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $dst "BepInEx\patchers") | Out-Null
New-Item -ItemType Directory -Force -Path (Join-Path $dst "BepInEx\config") | Out-Null

# Doorstop loader at the profile root (needed to launch modded).
foreach ($f in @("winhttp.dll", "doorstop_config.ini", ".doorstop_version")) {
    $s = Join-Path $src $f
    if (Test-Path $s) { Copy-Item $s (Join-Path $dst $f) -Force }
}

# BepInEx core (the loader runtime). Never copy the source profile's plugins.
$srcCore = Join-Path $src "BepInEx\core"
if (-not (Test-Path $srcCore)) { throw "Source profile has no BepInEx\core: $srcCore" }
Copy-Item $srcCore (Join-Path $dst "BepInEx") -Recurse -Force

# A ConfigurationManager is handy for testing; copy it if the source has it.
$cfgMgr = Get-ChildItem (Join-Path $src "BepInEx\plugins") -Recurse -Filter "ConfigurationManager*.dll" -ErrorAction SilentlyContinue | Select-Object -First 1
if ($cfgMgr) {
    $cmDir = Join-Path $dst "BepInEx\plugins\ConfigurationManager"
    New-Item -ItemType Directory -Force -Path $cmDir | Out-Null
    Copy-Item $cfgMgr.FullName $cmDir -Force
}

Write-Host "Test profile ready: $dst"
Write-Host "Deploy our plugins into it with:  make deploy"
Write-Host "Launch it with:                   make run-modded   (or select '$Name' in Thunderstore Mod Manager)"
