# Persistence

The server stores its durable state in SQLite, modeled natively. It never parses
a game save, because the game save format is a C#-only binary serialization that
is hostile to read from another language and would couple the server to the
game's internal layout.

## The rule: model, do not parse

The game persists its own world through its own save container. The server does
not read that file. Instead it models the shared concepts it needs as its own
tables and owns them authoritatively. A player's client still saves the game
locally as usual; the server's database is a separate, server-owned store of the
shared world.

This keeps the boundary clean: the game update that changes the save format
cannot corrupt the server, and the server schema evolves on its own migration
track.

## What the server persists

The server models the shared, durable slice of the world:

- Players: identity, a balance, and last-seen bookkeeping.
- Moorings: boats moored at a location that persist while their owner is
  offline and survive a server restart, with position, rotation, and ownership.
- Ledger: economy transactions, keyed by transaction id so a replay is
  idempotent and a balance never goes negative.
- World: the clock epoch and the weather seed, so the shared sky is restored on
  restart rather than reset.

## What maps from the game, conceptually

The game's save container holds analogous concepts: port demands and market
supply, currency rates, wind and wave state, day and time and moon phase, and
NPC boat data. The server draws its schema from those concepts (a shared clock,
a weather seed, an economy state, persistent boats) without reading the game's
bytes for any of them. The concrete field names on the game side are recorded in
`docs/04-reference/game-facts.md`; the mapping is by concept, not by
deserialization.

## Storage mechanics

- SQLite in write-ahead-logging mode, so reads do not block the tick loop's
  writes.
- Schema versioning through a stored version number, with migrations applied at
  boot from embedded SQL, so a fresh database and an upgraded one converge to
  the same schema.
- Synchronous access, because the server's tick loop is synchronous and an
  async database driver would add a runtime for no benefit at this scale.
- Writes flush on a fixed interval, on player disconnect, and on graceful
  shutdown, so a crash loses at most one interval of state.
