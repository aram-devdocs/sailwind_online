---
alwaysApply: true
---

# Layer discipline

- Dependencies flow one way, because a cycle between layers makes every change
  ripple unpredictably. The client plugin and the Rust server MUST NOT
  reference each other.
- `contracts/` is the only bridge between client and server, because the
  FlatBuffers schemas are the single shared vocabulary and a second bridge
  would drift from them.
- Apps and plugins stay thin composition layers, because logic that lives in a
  composition root cannot be unit-tested game-free.
- Game-touching code lives only in the designated engine-facing projects
  (`apps/Sailwind.API`, `apps/Sailwind.Online.Client`, and the game-coupled
  `packages/api-adapters`), because every other project — the pure packages
  under `packages/` — MUST build in CI without game IP present.
