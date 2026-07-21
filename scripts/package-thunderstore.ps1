# Stage and (optionally) publish a Thunderstore package for Sailwind.API or
# Sailwind Online. build/Versions.props is the single source of version truth.
#
#   scripts/package-thunderstore.ps1 -Package api
#   scripts/package-thunderstore.ps1 -Package client -Publish
#
# The script builds the plugin, stages manifest.json + icon.png + README (+ the
# plugin DLLs), prints the full staged listing for a game-IP audit, and HARD
# FAILS if any staged DLL is a game/Unity/BepInEx assembly. `tcli build` always
# runs; `tcli publish` runs only when TCLI_AUTH_TOKEN is set (so the release
# pipeline stays green before secrets exist). Requires lib/ to be provisioned.
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateSet('api', 'client')]
    [string]$Package,

    [switch]$Publish
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
Set-Location $repoRoot

# Assemblies that must never enter a package (game IP). Matched by name prefix.
$forbidden = @('Assembly-CSharp', 'UnityEngine', 'Crest', 'BepInEx', '0Harmony')

if (-not (Test-Path 'lib/Assembly-CSharp.dll')) {
    throw "lib/Assembly-CSharp.dll is absent. Run scripts/setup-game before packaging."
}

# --- version of truth ---------------------------------------------------------
[xml]$versions = Get-Content 'build/Versions.props'
$props = $versions.Project.PropertyGroup
$apiVersion = "$($props.SailwindApiVersion)".Trim()
$clientVersion = "$($props.SailwindOnlineVersion)".Trim()

if ($Package -eq 'api') {
    $version = $apiVersion
    $project = 'apps/Sailwind.API/Sailwind.API.csproj'
    $pluginDir = 'Sailwind.API'
    $sourceDir = 'thunderstore/api'
    $extraDlls = @()
} else {
    $version = $clientVersion
    $project = 'apps/Sailwind.Online.Client/Sailwind.Online.Client.csproj'
    $pluginDir = 'Sailwind.Online'
    $sourceDir = 'thunderstore/client'
    $extraDlls = @('Sailwind.Contracts.dll', 'LiteNetLib.dll')
}

if ([string]::IsNullOrWhiteSpace($version)) {
    throw "could not read the version for '$Package' from build/Versions.props."
}
Write-Host "Packaging $Package version $version"

# --- build the plugin ---------------------------------------------------------
$targetPath = (dotnet build $project -c Release -getProperty:TargetPath).Trim()
if ($LASTEXITCODE -ne 0 -or -not (Test-Path $targetPath)) {
    throw "plugin build did not produce a TargetPath ($targetPath)."
}
$targetDir = Split-Path -Parent $targetPath

# --- stage --------------------------------------------------------------------
$stage = "artifacts/thunderstore/$Package"
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force -Path $stage | Out-Null
New-Item -ItemType Directory -Force -Path "$stage/plugins/$pluginDir" | Out-Null

# manifest.json with version_number (and the client's API dependency) synced.
$manifest = Get-Content "$sourceDir/manifest.json" -Raw | ConvertFrom-Json
$manifest.version_number = $version
if ($Package -eq 'client') {
    $manifest.dependencies = @($manifest.dependencies | ForEach-Object {
        if ($_ -like 'aram_devdocs-SailwindAPI-*') { "aram_devdocs-SailwindAPI-$apiVersion" } else { $_ }
    })
}
$manifest | ConvertTo-Json -Depth 8 | Set-Content "$stage/manifest.json" -Encoding utf8

Copy-Item "$sourceDir/README.md" "$stage/README.md"
foreach ($optional in @('icon.png', 'CHANGELOG.md')) {
    if (Test-Path "$sourceDir/$optional") {
        Copy-Item "$sourceDir/$optional" "$stage/$optional"
    } else {
        Write-Warning "$sourceDir/$optional is missing; tcli build will require it before a real publish."
    }
}

# Plugin DLL plus the client's private dependencies.
Copy-Item $targetPath "$stage/plugins/$pluginDir/"
foreach ($dll in $extraDlls) {
    $src = Join-Path $targetDir $dll
    if (-not (Test-Path $src)) {
        throw "expected dependency '$dll' not found in $targetDir."
    }
    Copy-Item $src "$stage/plugins/$pluginDir/"
}

# --- audit --------------------------------------------------------------------
Write-Host "Staged files:"
Get-ChildItem -Recurse -File $stage | ForEach-Object {
    Write-Host "  $($_.FullName.Substring($repoRoot.Length + 1))"
}

$stagedDlls = Get-ChildItem -Recurse -File -Filter '*.dll' $stage
foreach ($dll in $stagedDlls) {
    foreach ($bad in $forbidden) {
        if ($dll.Name -like "$bad*") {
            throw "game IP leak: '$($dll.Name)' matches forbidden prefix '$bad'. Aborting."
        }
    }
}
Write-Host "Audit: no forbidden assemblies staged."

# --- thunderstore.toml + tcli -------------------------------------------------
$namespace = 'aram_devdocs'
$toml = @"
[config]
schemaVersion = "0.0.1"

[package]
namespace = "$namespace"
name = "$($manifest.name)"
versionNumber = "$version"
description = "$($manifest.description)"
websiteUrl = "$($manifest.website_url)"
containsNsfwContent = false

[package.dependencies]
"@
foreach ($dep in $manifest.dependencies) {
    $parts = $dep -split '-'
    $depVersion = $parts[-1]
    $depName = ($parts[0..($parts.Length - 2)] -join '-')
    $toml += "`n`"$depName`" = `"$depVersion`""
}
$toml += @"


[build]
icon = "./icon.png"
readme = "./README.md"
outdir = "./build"

[[build.copy]]
source = "./plugins"
target = "./plugins"

[publish]
repository = "https://thunderstore.io"
communities = ["sailwind"]
"@
Set-Content "$stage/thunderstore.toml" $toml -Encoding utf8

if (-not (Get-Command tcli -ErrorAction SilentlyContinue)) {
    Write-Warning "tcli not on PATH; staged package is at $stage but was not built. Install the Thunderstore CLI to build/publish."
    return
}

Push-Location $stage
try {
    tcli build
    if ($LASTEXITCODE -ne 0) { throw "tcli build failed." }

    if ($Publish) {
        if ([string]::IsNullOrEmpty($env:TCLI_AUTH_TOKEN)) {
            Write-Host "TCLI_AUTH_TOKEN not set; built package but skipping publish. Exiting green."
        } else {
            tcli publish --token $env:TCLI_AUTH_TOKEN
            if ($LASTEXITCODE -ne 0) { throw "tcli publish failed." }
        }
    }
} finally {
    Pop-Location
}

Write-Host "Thunderstore packaging for $Package complete."
