# Fail if any game IP or build binary is tracked in git. Game DLLs, executables,
# debug symbols, and anything under lib/ or .sandbox/ must never be committed.
# Mirror of guard-no-game-ip.sh for Windows.
$ErrorActionPreference = 'Stop'

$tracked = git ls-files -- `
  '*.dll' '*.exe' '*.pdb' `
  'lib/**' 'lib/*' `
  '.sandbox/**' '.sandbox/*'

if ($LASTEXITCODE -ne 0) {
    throw "git ls-files failed (is this a git repository?)"
}

$tracked = $tracked | Where-Object { $_ -ne '' }

if ($tracked) {
    Write-Error "game IP or binaries are tracked in git:`n$($tracked -join "`n")`n`nRemove them and confirm lib/, .sandbox/, *.dll, *.exe, *.pdb are gitignored."
    exit 1
}

Write-Host "guard-no-game-ip: clean (no tracked game DLLs, binaries, or sandbox)."
