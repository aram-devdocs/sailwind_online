---
description: Server logic that affects shared state must be deterministic, because clients replay that state and must reach the same result.
globs: ["server/**"]
---

# Determinism

Clients replay server state and must agree byte for byte. Nondeterminism on the
server desyncs them.

- Server logic that affects shared state MUST NOT branch on wall-clock time
  (`SystemTime::now`, `Instant::now`) for anything broadcast or hashed, because
  two runs at different instants would then diverge. Drive time from the
  simulation tick that every client already shares.
- Iteration over a `HashMap` or `HashSet` MUST NOT feed anything that is
  hashed, serialized, or broadcast, because their order is randomized per
  process and clients would compute different bytes. Use a `BTreeMap`, a sorted
  key list, or an insertion-ordered structure for those paths.
- Randomness that affects shared state MUST come from a seeded generator whose
  seed is part of replicated state, because an unseeded RNG cannot be replayed.
- Floating-point results that cross the wire SHOULD be avoided in favor of
  integer or fixed-point math, because float rounding differs across platforms
  and small divergences accumulate into desync.

Local-only server state (logging, metrics, debug counters) is exempt because it
is never replayed by a client.
