#!/usr/bin/env bash
# Package the built sw-server binary into a per-target release archive.
#
#   scripts/package-server.sh <rust-target> <tar.gz|zip>
#
# Bundles the binary, LICENSE, a sample config, and a short SERVER.md. Output
# lands in artifacts/release/. Used by the server-v* release workflow on both
# Linux (tar.gz) and Windows (zip).
set -euo pipefail

target="${1:?usage: package-server.sh <target> <tar.gz|zip>}"
archive="${2:?usage: package-server.sh <target> <tar.gz|zip>}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

bin="sw-server"
if [ "$archive" = "zip" ]; then
  bin="sw-server.exe"
fi

bin_path="server/target/${target}/release/${bin}"
if [ ! -f "$bin_path" ]; then
  echo "error: built binary not found at $bin_path" >&2
  exit 1
fi

name="sailwind-online-server-${target}"
stage="artifacts/release/${name}"
rm -rf "$stage"
mkdir -p "$stage"

cp "$bin_path" "$stage/"
cp LICENSE "$stage/LICENSE"

if [ -f server/config.example.toml ]; then
  cp server/config.example.toml "$stage/config.example.toml"
else
  echo "warning: server/config.example.toml missing; omitting sample config." >&2
fi

cat > "$stage/SERVER.md" <<'EOF'
# Sailwind Online server

Standalone authoritative server for Sailwind Online. It holds thin authority:
presence, moorage and persistent boats, a shared economy ledger, and the world
clock and weather seed. Heavy sailing simulation stays on the game clients.

## Run

1. Copy `config.example.toml` to `config.toml` and edit the bind address, the
   SQLite database path, and the server name.
2. Start the server:

       ./sw-server --config config.toml

   The server prints `listening on <addr>` once it is ready to accept clients.

## Notes

- Default UDP bind is `0.0.0.0:38455`. Clients connect with the connect key
  `sailwind-online`.
- State persists to the configured SQLite database and survives restarts.
- This build speaks protocol version 1. Clients on a different protocol version
  are refused at handshake.
EOF

mkdir -p artifacts/release
out="artifacts/release/${name}.${archive}"
rm -f "$out"

case "$archive" in
  tar.gz)
    tar -czf "$out" -C artifacts/release "$name"
    ;;
  zip)
    if command -v zip >/dev/null 2>&1; then
      ( cd artifacts/release && zip -r "${name}.zip" "$name" >/dev/null )
    else
      powershell -NoProfile -Command "Compress-Archive -Path 'artifacts/release/${name}/*' -DestinationPath 'artifacts/release/${name}.zip' -Force"
    fi
    ;;
  *)
    echo "error: unknown archive type '$archive' (expected tar.gz or zip)" >&2
    exit 1
    ;;
esac

echo "packaged: $out"
ls -l "$out"
