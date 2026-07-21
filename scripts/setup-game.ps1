<#
.SYNOPSIS
  Populate lib/ with the Sailwind game assemblies + BepInEx core so the engine-facing
  plugins (Sailwind.API, Sailwind.Online.Client) and ApiGen can build against them.

.DESCRIPTION
  Game IP never enters the repo: lib/ is gitignored. Resolution order for the game
  directory is -GameDir -> $env:SAILWIND_GAME_DIR -> Steam (registry SteamPath +
  libraryfolders.vdf for appid 1764530).

.NOTES
  Windows-only. Re-runnable (overwrites lib/).
#>
[CmdletBinding()]
param(
    [string]$GameDir,
    [string]$BepInExDir,
    [string]$BepInExVersion = "5.4.23.2"
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$AppId = "1764530"
$RepoRoot = Split-Path $PSScriptRoot -Parent
$Lib = Join-Path $RepoRoot "lib"
$Tools = Join-Path $RepoRoot ".tools"

function Resolve-SteamPath {
    $key = "HKCU:\Software\Valve\Steam"
    if (Test-Path $key) {
        $p = (Get-ItemProperty -Path $key -Name "SteamPath" -ErrorAction SilentlyContinue).SteamPath
        if ($p) { return $p.Replace("/", "\") }
    }
    return $null
}

function Get-SteamLibraryRoots([string]$steamPath) {
    $roots = @($steamPath)
    $vdf = Join-Path $steamPath "steamapps\libraryfolders.vdf"
    if (Test-Path $vdf) {
        foreach ($m in [regex]::Matches((Get-Content $vdf -Raw), '"path"\s+"([^"]+)"')) {
            $roots += $m.Groups[1].Value.Replace("\\", "\")
        }
    }
    return $roots | Select-Object -Unique
}

function Resolve-GameDir {
    if ($GameDir) { return $GameDir }
    if ($env:SAILWIND_GAME_DIR) { return $env:SAILWIND_GAME_DIR }

    $steam = Resolve-SteamPath
    if (-not $steam) { throw "Steam not found in the registry; pass -GameDir <path-to-Sailwind>." }

    foreach ($root in Get-SteamLibraryRoots $steam) {
        $manifest = Join-Path $root "steamapps\appmanifest_$AppId.acf"
        $common = Join-Path $root "steamapps\common\Sailwind"
        if ((Test-Path $manifest) -and (Test-Path $common)) { return $common }
    }
    throw "Could not locate the Sailwind install (appid $AppId). Pass -GameDir <path>."
}

function Resolve-BuildId([string]$gameDir) {
    $steam = Resolve-SteamPath
    if ($steam) {
        foreach ($root in Get-SteamLibraryRoots $steam) {
            $manifest = Join-Path $root "steamapps\appmanifest_$AppId.acf"
            if (Test-Path $manifest) {
                $m = [regex]::Match((Get-Content $manifest -Raw), '"buildid"\s+"(\d+)"')
                if ($m.Success) { return $m.Groups[1].Value }
            }
        }
    }
    return "unknown"
}

function Copy-One([string]$src, [string]$dstDir) {
    if (-not (Test-Path $src)) { throw "expected game file not found: $src" }
    Copy-Item -Path $src -Destination $dstDir -Force
    Write-Host "  copied $(Split-Path $src -Leaf)"
}

function Resolve-BepInExCore {
    if ($BepInExDir) { return $BepInExDir }

    $tmm = Join-Path $env:APPDATA "Thunderstore Mod Manager\DataFolder\Sailwind\profiles\Default\BepInEx\core"
    if (Test-Path (Join-Path $tmm "BepInEx.dll")) { return $tmm }

    # Fallback: download the pinned BepInEx x64 release into .tools/.
    $zipName = "BepInEx_x64_$BepInExVersion.zip"
    $url = "https://github.com/BepInEx/BepInEx/releases/download/v$BepInExVersion/$zipName"
    $extractDir = Join-Path $Tools "BepInEx_$BepInExVersion"
    if (-not (Test-Path (Join-Path $extractDir "BepInEx\core\BepInEx.dll"))) {
        New-Item -ItemType Directory -Force -Path $Tools | Out-Null
        $zipPath = Join-Path $Tools $zipName
        Write-Host "Downloading BepInEx $BepInExVersion ..."
        Invoke-WebRequest -Uri $url -OutFile $zipPath
        Expand-Archive -Path $zipPath -DestinationPath $extractDir -Force
    }
    return Join-Path $extractDir "BepInEx\core"
}

# --- run -------------------------------------------------------------------

$game = Resolve-GameDir
$managed = Join-Path $game "Sailwind_Data\Managed"
if (-not (Test-Path $managed)) { throw "Managed folder not found under $game" }

New-Item -ItemType Directory -Force -Path $Lib | Out-Null
Write-Host "Game:  $game"
Write-Host "Lib:   $Lib"

Write-Host "Copying game assemblies..."
Copy-One (Join-Path $managed "Assembly-CSharp.dll") $Lib
Copy-One (Join-Path $managed "Assembly-CSharp-firstpass.dll") $Lib
Copy-One (Join-Path $managed "Crest.dll") $Lib
foreach ($dll in Get-ChildItem -Path $managed -Filter "UnityEngine*.dll") {
    Copy-Item -Path $dll.FullName -Destination $Lib -Force
}
Write-Host "  copied UnityEngine*.dll set"

Write-Host "Copying BepInEx core..."
$core = Resolve-BepInExCore
Copy-One (Join-Path $core "BepInEx.dll") $Lib
Copy-One (Join-Path $core "0Harmony.dll") $Lib

$buildId = Resolve-BuildId $game
Set-Content -Path (Join-Path $Lib "game-build.txt") -Value $buildId -Encoding utf8 -NoNewline
Write-Host "Wrote lib/game-build.txt = $buildId"

Write-Host "setup-game complete."
