<#
.SYNOPSIS
  Start the Sailwind Online server and launch Sailwind modded from the dedicated
  SailwindOnline test profile (never the user's Default profile).

.DESCRIPTION
  1. Builds and starts the Rust server (sw-server) unless -NoServer.
  2. Points the game's Unity Doorstop loader at the test profile's BepInEx
     preloader, launches via Steam (so Steam's DRM re-exec keeps the loader),
     waits for BepInEx to come up, then restores the game's Doorstop config.

  The env-var approach does not work for Sailwind because the game re-execs
  through Steam and drops the vars; redirecting the game-dir doorstop_config.ini
  is the reliable method (verified against Doorstop 4.5.0).
#>
[CmdletBinding()]
param(
    [string]$Profile = "SailwindOnline",
    [string]$GameDir,
    [switch]$NoServer
)
$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$AppId = "1764530"
$repoRoot = Split-Path -Parent $PSScriptRoot
$profileDir = Join-Path $env:APPDATA "Thunderstore Mod Manager\DataFolder\Sailwind\profiles\$Profile"
$preloader = Join-Path $profileDir "BepInEx\core\BepInEx.Preloader.dll"
$profileLog = Join-Path $profileDir "BepInEx\LogOutput.log"

if (-not (Test-Path $preloader)) {
    throw "Test profile preloader not found: $preloader`nRun: make test-profile ; make deploy"
}

# Resolve the game directory, guarding against library entries on drives that are
# not currently mounted (a Steam library on an absent drive would otherwise throw).
function Resolve-GameDir {
    param([string]$Override)
    if ($Override -and (Test-Path (Join-Path $Override "Sailwind.exe"))) { return $Override }
    $steam = (Get-ItemProperty "HKCU:\Software\Valve\Steam" -Name SteamPath -ErrorAction SilentlyContinue).SteamPath
    if (-not $steam) { return $null }
    $roots = New-Object System.Collections.Generic.List[string]
    $roots.Add(($steam -replace '/', '\'))
    $vdf = Join-Path $steam "steamapps\libraryfolders.vdf"
    if (Test-Path $vdf) {
        foreach ($m in [regex]::Matches((Get-Content -Raw $vdf), '"path"\s+"([^"]+)"')) {
            $p = $m.Groups[1].Value -replace '\\\\', '\'
            $drive = [System.IO.Path]::GetPathRoot($p)
            if ($drive -and (Test-Path $drive)) { $roots.Add($p) }
        }
    }
    foreach ($r in $roots) {
        $candidate = Join-Path $r "steamapps\common\Sailwind"
        if (Test-Path (Join-Path $candidate "Sailwind.exe")) { return $candidate }
    }
    return $null
}

$GameDir = Resolve-GameDir -Override $GameDir
if (-not $GameDir) { throw "Could not locate Sailwind.exe. Pass -GameDir <path>." }
$doorstopCfg = Join-Path $GameDir "doorstop_config.ini"

# 1. Server
if (-not $NoServer) {
    Write-Host "Building + starting the server..."
    & cargo build --release --manifest-path (Join-Path $repoRoot "server\Cargo.toml")
    if ($LASTEXITCODE -ne 0) { throw "server build failed" }
    Start-Process -FilePath (Join-Path $repoRoot "server\target\release\sw-server.exe") -WorkingDirectory $repoRoot
    Write-Host "Server started (listening on 0.0.0.0:38455)."
}

# 2. Redirect the game's Doorstop to the test profile, launch, wait, restore.
$backup = "$doorstopCfg.swobak"
Copy-Item $doorstopCfg $backup -Force
try {
    $preFwd = $preloader -replace '\\', '/'
    (Get-Content $doorstopCfg) -replace '^target_assembly=.*', "target_assembly=$preFwd" | Set-Content $doorstopCfg -Encoding ascii
    if (Test-Path $profileLog) { Remove-Item $profileLog -Force }

    Write-Host "Launching Sailwind modded from profile '$Profile' via Steam..."
    Start-Process "steam://rungameid/$AppId"

    Write-Host "Waiting for BepInEx to load the test profile..."
    $loaded = $false
    for ($i = 0; $i -lt 40; $i++) {
        Start-Sleep -Seconds 3
        if ((Test-Path $profileLog) -and (Select-String -Path $profileLog -Pattern 'BepInEx .* Sailwind' -Quiet)) { $loaded = $true; break }
    }
    if ($loaded) {
        Write-Host "BepInEx loaded. Recent plugin lines:"
        Select-String -Path $profileLog -Pattern 'Loading \[|Surface check|Sailwind Online|Connected' | Select-Object -Last 6 | ForEach-Object { "  $($_.Line)" }
    } else {
        Write-Warning "Did not see BepInEx in $profileLog within the timeout; check the log manually."
    }
}
finally {
    # Restore the game's Doorstop config now that it has been read at launch.
    Move-Item $backup $doorstopCfg -Force
    Write-Host "Restored the game's Doorstop config. Default profile launches are unaffected."
}
