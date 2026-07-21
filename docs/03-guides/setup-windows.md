# Setup on Windows

This repository is Windows-first, because the game and its Thunderstore mod
manager run on Windows and the game-coupled build needs the local install. The
game-free build and the Rust server run anywhere; only provisioning game
references and deploying into the game are Windows steps.

Concrete tool versions and install paths live in
`docs/04-reference/game-facts.md`. This guide stays at the level of what to do,
not which build number.

## Prerequisites

- The .NET SDK, for the C# projects and tools.
- The Rust toolchain (via rustup), for the server workspace.
- The FlatBuffers compiler, pinned; the contracts scripts download the pinned
  build for you, so a manual install is optional.
- Git with Git Bash, because the hooks and shell scripts run under bash.
- For the game-coupled build only: a Steam copy of Sailwind and the Thunderstore
  Mod Manager with a mod profile, so setup can find the game assemblies and
  BepInEx core, and deploy can copy the plugin into a running install.

## Provisioning

Run `make setup`. It locates the Steam install of the game, mirrors what it
needs into the gitignored sandbox and `lib/` directories, and pulls the BepInEx
core assemblies from your mod profile. Game files never enter git; `lib/` and
the sandbox are gitignored by design.

On a machine without the game installed, `make setup` fails with a clear
message rather than half-provisioning. That is expected: the game-free build
below does not need it.

## Building

- `make check` is the fast gate: it builds the game-free projects with warnings
  as errors and runs the game-free test suites plus the Rust checks. It needs no
  game IP and is what CI runs.
- `make build` builds the full solution, including the game-coupled plugins, and
  requires `lib/` from setup.
- `make audit` runs the full dashboard, including the game-coupled test suites
  when the game assemblies are present locally.

## Sandbox and deploy

Setup produces a sandbox: a local mirror of the pieces the plugins need to
build and a target the deploy step writes into. `make deploy` builds the plugins
and copies them into your Thunderstore mod profile, so launching the game
through the mod manager loads your freshly built plugin. A build-time switch
disables the copy when you want to build without deploying.

## Running the server

`make server-run` builds and starts the Rust server locally. The protocol
conformance harness (`make smoke`) spawns its own server, so you do not need a
running server to exercise the wire protocol end to end without the game.
