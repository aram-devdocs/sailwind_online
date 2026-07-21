# Monorepo structure

The repository groups code by language and by role, with one rule that governs
every dependency: it flows one way, and the contracts layer is the only bridge
between the client and the server. There is no `src/` directory. Code lives
under `packages/` (the libraries that carry behaviour) and `apps/` (the thin
plugins that compose them).

## Top-level layout

- `contracts/` is the single source of truth for the wire. It holds the
  FlatBuffers schemas (`fbs/`), the committed generated C# bindings (`cs/`), and
  the pinned toolchain metadata. Both the client and the server read their
  message types from here, so neither hand-writes a codec.
- `packages/` holds the C# libraries, split by whether they touch the game:
  - `api-abstractions` holds the pure mod-API surface: the game-facing
    interfaces (clock, wind, player boat, save events), the `BoatPose` value
    type built on `System.Numerics`, and the `SailwindApi` facade that other
    mods consume. It carries no game reference.
  - `net` holds the transport and wire codec: the LiteNetLib client and the
    FlatBuffers `Codec` over the contracts bindings.
  - `sync` holds the client-side sync layer: the `SnapshotCache` and the
    `StateReporter`.
  - `api-adapters` is the one game-coupled library: the reflection and Harmony
    adapters that bind the abstractions to the real game assembly, plus the
    generated `GameRef` and `SurfaceManifest` seam the codegen emits.
  The first three build without any game reference; only `api-adapters` does.
- `apps/` holds the two thin BepInEx plugins that compose those packages:
  `Sailwind.API` (the contract-layer plugin over the game) and
  `Sailwind.Online.Client` (the multiplayer client plugin). The apps and
  `api-adapters` are the only C# projects that reference game assemblies, and
  the apps hold composition, not logic.
- `server/` holds the Rust cargo workspace: the authoritative server and its
  supporting crates (networking, world grid, economy, persistence, contracts
  bindings).
- `tools/` holds build-time and test-time programs: the Cecil-based API code
  generator and the protocol conformance harness.
- `tests/` holds the test projects, split into a game-free set that always runs
  in CI and a game-coupled set that runs only where game assemblies are present.
  `Sailwind.Architecture.Tests` is the game-free enforcer of the dependency DAG
  described below.
- `docs/` holds this documentation.
- `scripts/` holds the setup and gate scripts, each as a paired PowerShell and
  shell implementation.
- `.agents/` is the single source of truth for agent configuration: `rules/`
  (each backed by a validator), `skills/` (including `/work`), `runs/` (durable
  run state), and `lessons-learned.md`. `.claude/` symlinks `rules/` and
  `skills/` back into `.agents/` and adds only the Claude-specific pieces:
  `agents/` (the subagent roster), `hooks/` (the governance hooks), and
  `settings.json`. `AGENTS.md` is canonical, and every `CLAUDE.md` is a symlink
  to it.

## Language grouping

C# spans target frameworks by role: the game-free libraries target the portable
common denominator both runtimes load, the engine-facing projects (`api-adapters`
and the two apps) compile against the game's framework, and the tools and tests
target the current .NET SDK. Rust lives entirely under `server/` as one cargo
workspace. The exact framework and toolchain versions live in
`docs/04-reference/`.

## Dependency rules

The layering is a one-way directed acyclic graph, and it is machine-enforced,
not only described here.

- The client plugin and the server MUST NOT reference each other, because a
  direct link would let one leak assumptions into the other and give the wire
  format two sources of truth.
- `contracts/` MUST be the only bridge between them, because a message both
  sides decode has to come from one schema, not two hand-written codecs.
- Every C# project except `api-adapters` and the two apps MUST build game-free,
  because CI holds no game IP.
- Apps and plugins MUST stay thin composition roots, because logic in a
  composition root cannot be tested game-free.

These rules are enforced by `tests/Sailwind.Architecture.Tests`, which reads the
compiled assemblies with Mono.Cecil and asserts the allowed edges; its own
synthetic bite-tests prove the check fails when a banned edge is introduced, so
the guard cannot rot into a no-op. The Rust side has an equivalent crate-DAG
test over the cargo workspace. A violation fails the build, so the DAG stays
honest.

## Why one repository

The contracts layer and its two consumers change together often enough that a
split into separate repositories would turn one PR into a coordinated release
across three. A monorepo keeps a schema change and both regenerated bindings in
a single reviewable diff.
