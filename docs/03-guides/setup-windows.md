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
  Mod Manager with a mod profile, so setup can find the game assemblies and the
  BepInEx core.

## Provisioning

Run `make setup`. It locates the Steam install of the game, copies the game
assemblies and the BepInEx core it needs into the gitignored `lib/` directory,
and records the Steam build id there. Game files never enter git; `lib/` is
gitignored by design, so nothing derived from game IP is ever committed.

On a machine without the game installed, `make setup` fails with a clear message
rather than half-provisioning. That is expected: the game-free gate below does
not need it.

## Validating and building

- `make validate` is the canonical gate and mirrors CI. It runs the convention
  and leak checks, the game-IP guard, the governance-hook fixtures, the
  warnings-as-errors game-free build, every game-free test suite, and the Rust
  format, lint, and test steps. Where `lib/` is present it also builds the full
  solution and runs the game-coupled surface tests. Run it before every push,
  because a green run locally is defined to mean a green pipeline.
- `make build` builds the full solution in Release, including the game-coupled
  plugins, and needs `lib/` from setup.
- `make build-ci` builds the game-free solution filter only, which is what a
  machine without game IP can build.

## Test profile and deploy

Deploying into the game is opt-in and always isolated from your own mods.

- `make test-profile` creates a dedicated `SailwindOnline` Thunderstore mod
  profile next to your own. It copies the BepInEx core and loader from an
  existing profile into a fresh one with an empty plugins folder, so your own
  profile and its mod set are never touched.
- `make deploy` builds Release with the deploy switch on (`-p:Deploy=true`) and
  copies the freshly built plugins into that `SailwindOnline` profile. The
  switch is off by default, so an ordinary build never writes into a game
  profile.
- `make run-modded` starts the local server and launches Sailwind modded from
  the `SailwindOnline` profile, so you can exercise the plugins end to end
  without disturbing your normal modded game.

## Running the server

`make server-run` builds and starts the Rust server locally. The protocol
conformance harness (`make smoke`) spawns its own server, so you do not need a
running server to exercise the wire protocol end to end without the game.
