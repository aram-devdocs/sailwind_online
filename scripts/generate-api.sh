#!/usr/bin/env bash
# Run Sailwind.ApiGen against lib/Assembly-CSharp.dll: verify the seed manifest and
# regenerate GameRef.g.cs, api-surface.json (+ .sha256), SurfaceManifest.g.cs and
# SurfaceContract.g.cs. Requires lib/ (run scripts/setup-game.ps1 first).
#
# ApiGen itself is game-free (Cecil reads the DLL as a file), so this mirror runs on
# any OS as long as lib/Assembly-CSharp.dll is present.
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
assembly="$repo_root/lib/Assembly-CSharp.dll"

if [[ ! -f "$assembly" ]]; then
    echo "lib/Assembly-CSharp.dll not found. Run scripts/setup-game.ps1 first." >&2
    exit 1
fi

cd "$repo_root"
exec dotnet run --project "$repo_root/tools/Sailwind.ApiGen" -c Release
