# Monorepo structure

The repository groups code by language and by role, with one rule that governs
every dependency: it flows one way, and the contracts layer is the only bridge
between the client and the server.

## Top-level layout

- `contracts/` holds the FlatBuffers schemas (`fbs/`), the generated C# runtime
  and bindings (`cs/`), and the pinned toolchain metadata. This is the single
  shared vocabulary between client and server.
- `src/` holds the engine-facing C# plugins: `Sailwind.API` (the contract layer
  over the game) and `Sailwind.Online.Client` (the multiplayer client plugin).
  These are the only projects that reference game assemblies.
- `server/` holds the Rust cargo workspace: the authoritative server and its
  supporting crates (networking, world grid, economy, persistence, contracts
  bindings).
- `tools/` holds build-time and test-time programs: the Cecil-based API code
  generator and the protocol conformance harness.
- `tests/` holds the test projects, split into a game-free set that always runs
  in CI and a game-coupled set that runs only where game assemblies are present.
- `docs/` holds this documentation.
- `scripts/` holds the setup and gate scripts, each as a paired PowerShell and
  shell implementation.
- `.claude/` holds the committed agent configuration: rules, skills, and the
  lessons-learned whiteboard.

## Language grouping

C# spans three target frameworks by role: the engine-facing plugins compile
against the game's Mono runtime, the contracts assembly targets the common
denominator both worlds consume, and the tools and tests target the current
.NET SDK. Rust lives entirely under `server/` as one cargo workspace. The exact
framework and toolchain versions live in `docs/04-reference/`.

## Dependency rules

- The client plugin and the server MUST NOT reference each other, because a
  direct link would let one leak assumptions into the other and there would be
  two sources of truth for the wire format.
- `contracts/` is the only bridge, because a message that both sides understand
  MUST come from one schema, not two hand-written codecs.
- Engine-facing projects import the game references; every other project MUST
  build without them, because CI has no game IP.
- Apps and plugins stay thin composition layers, because logic in a composition
  root cannot be tested game-free.

## Why one repository

The contracts layer and its two consumers change together often enough that a
split into separate repositories would turn one PR into a coordinated release
across three. A monorepo keeps a schema change and both regenerated bindings in
a single reviewable diff.
