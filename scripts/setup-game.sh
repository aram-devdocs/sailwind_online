#!/usr/bin/env bash
# Windows-only step. Populating lib/ reads a Steam install of Sailwind and the
# BepInEx core from a Thunderstore Mod Manager profile, both Windows-specific.
# The parity .sh mirror exists so the pair is discoverable; it does not run.
set -euo pipefail

cat >&2 <<'MSG'
setup-game: Windows-only step.

  Populating lib/ requires the Windows Steam install of Sailwind and a
  Thunderstore Mod Manager BepInEx profile. Run the PowerShell version:

      pwsh scripts/setup-game.ps1 [-GameDir <path-to-Sailwind>]

  On a non-Windows machine, copy these into lib/ manually from a Windows box:
      Assembly-CSharp.dll  Assembly-CSharp-firstpass.dll  Crest.dll
      UnityEngine*.dll  BepInEx.dll  0Harmony.dll
  and write the Steam buildid into lib/game-build.txt.
MSG
exit 1
