<#
.SYNOPSIS
  Start the Sailwind Online server and launch Sailwind modded from the dedicated
  test profile (never the user's Default profile).

.DESCRIPTION
  1. Builds and starts the Rust server (sw-server) in a new window.
  2. Launches Sailwind with Unity Doorstop pointed at the SailwindOnline test
     profile's BepInEx preloader, so only our plugins load. Requires Steam to be
     running and the test profile to exist (make test-profile) with our plugins
     deployed (make deploy).

.NOTES
  Doorstop 4.5 is driven via environment variables so the game install is never
  modified. Verified against Doorstop 4.5.0.
#>
[CmdletBinding()]
param(
    [string]$Profile = "SailwindOnline",
    [string]$GameDir,
    [switch]$NoServer
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent $PSScriptRoot
$profileDir = Join-Path $env:APPDATA "Thunderstore Mod Manager\DataFolder\Sailwind\profiles\$Profile"
$preloader = Join-Path $profileDir "BepInEx\core\BepInEx.Preloader.dll"

if (-not (Test-Path $preloader)) {
    throw "Test profile preloader not found: $preloader`nRun: make test-profile ; make deploy"
}

# Resolve the game directory (registry -> libraryfolders.vdf), reusing setup-game's logic path.
if (-not $GameDir) {
    $steam = (Get-ItemProperty "HKCU:\Software\Valve\Steam" -Name SteamPath -ErrorAction SilentlyContinue).SteamPath
    if ($steam) {
        $candidates = @((Join-Path $steam "steamapps\common\Sailwind"))
        $vdf = Join-Path $steam "steamapps\libraryfolders.vdf"
        if (Test-Path $vdf) {
            foreach ($m in [regex]::Matches((Get-Content -Raw $vdf), '"path"\s+"([^"]+)"')) {
                $candidates += (Join-Path ($m.Groups[1].Value -replace '\\\\', '\') "steamapps\common\Sailwind")
            }
        }
        $GameDir = $candidates | Where-Object { Test-Path (Join-Path $_ "Sailwind.exe") } | Select-Object -First 1
    }
}
if (-not $GameDir -or -not (Test-Path (Join-Path $GameDir "Sailwind.exe"))) {
    throw "Could not locate Sailwind.exe. Pass -GameDir <path>."
}

# 1. Server
if (-not $NoServer) {
    Write-Host "Building + starting the server..."
    & cargo build --release --manifest-path (Join-Path $repoRoot "server\Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "server build failed" }
    $serverExe = Join-Path $repoRoot "server\target\release\sw-server.exe"
    Start-Process -FilePath $serverExe -WorkingDirectory $repoRoot
    Write-Host "Server started (listening on 0.0.0.0:38455)."
}

# 2. Launch the game modded via Doorstop env vars (game install untouched).
Write-Host "Launching Sailwind modded from profile '$Profile'..."
$env:DOORSTOP_ENABLED = "TRUE"
$env:DOORSTOP_TARGET_ASSEMBLY = $preloader
$env:DOORSTOP_MONO_DLL_SEARCH_PATH_OVERRIDE = ""
Start-Process -FilePath (Join-Path $GameDir "Sailwind.exe") -WorkingDirectory $GameDir

Write-Host ""
Write-Host "Watch the log for our plugins:"
Write-Host "  Get-Content -Wait '$profileDir\BepInEx\LogOutput.log'"
Write-Host "Expect: [Sailwind.API] Surface check OK (...)  and  [Sailwind Online] Connected ..."
