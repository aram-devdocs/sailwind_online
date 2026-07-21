---
description: The layer DAG. Packages define behavior, apps orchestrate it, contracts is the only client-server bridge, and the game is reached only through the adapters.
alwaysApply: true
---

# Dependency hierarchy

The build is a directed acyclic graph. Depend downward only.

- Packages under `packages/` hold all testable logic and MUST stay game-free
  (Sailwind.Contracts, Sailwind.Api.Abstractions, Sailwind.Online.Net,
  Sailwind.Online.Sync), because a package that pulls in game IP can no longer
  build in CI without the game present.
- Apps under `apps/` (Sailwind.API, Sailwind.Online.Client) are thin
  composition roots that wire packages together and MUST NOT hold logic worth a
  unit test, because logic in a composition root cannot be tested game-free.
- The client and the Rust server MUST NOT reference each other; `contracts/` is
  their only shared vocabulary, because a second bridge drifts from the
  FlatBuffers schemas and a drift is a wire break.
- Game members are reached only through `packages/Sailwind.Api.Adapters`
  (net472, game-coupled), because concentrating the coupling in one project
  keeps a game update a single failing test instead of a scattered break.
- Rust crates depend downward only: sw-contracts is the base, then sw-net,
  sw-world, sw-econ, sw-persist, then sw-server on top. A crate MUST NOT depend
  upward or sideways in a cycle, because a cycle makes every change ripple
  unpredictably.

Enforced by `tests/Sailwind.Architecture.Tests` (Cecil reads the compiled
edges) and the Rust crate-DAG test, both run by `make validate`.
