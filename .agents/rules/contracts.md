---
globs: ["contracts/**"]
---

# Contracts

- The `.fbs` schemas are the single source of truth for every client-server
  message, because two hand-written codecs drift and a drift is a wire break.
- Generated C# and Rust code is committed but MUST NOT be hand-edited, because
  the next regeneration silently overwrites the edit and CI diffs it back out.
- A schema change MUST regenerate both languages in the same PR, because a
  one-sided regeneration ships a client and server that disagree on the wire.
- Prefer additive schema evolution (new fields, new union members), because
  FlatBuffers keeps additive changes backward compatible and reordering does
  not.
