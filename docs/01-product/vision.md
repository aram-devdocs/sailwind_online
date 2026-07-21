# Vision

Sailwind Online is one repository that ships two products for the sailing game
Sailwind. They share a code-generation toolchain and a wire-contract layer, so
they live together, but they serve different people and can be adopted
separately.

## Sailwind.API

A code-generated contract layer over the game's compiled assembly. It gives
other mod authors a stable, typed seam into the game instead of the usual
hand-written reflection and magic strings. A tool reads the real game assembly,
checks every member the API touches against a declared manifest, and generates
interfaces, events, and a surface hash. When a game update moves or renames a
member, a contract test fails in our build rather than a user's install
breaking at runtime.

Who it is for: mod authors who want a target that survives game updates, or
fails loudly and early when it cannot.

## Sailwind.Online

A persistent-world multiplayer mod. A client plugin runs inside the game and
talks over UDP to a standalone server that holds thin authority. Players see
each other sail, moor boats that persist while they are offline, share one sky
and clock, and trade against a shared economy ledger.

Who it is for: players who want to sail the same world together.

## The split, and why it exists

The API layer has value on its own: any mod can build on it. The multiplayer
mod is the first and heaviest consumer of that layer, and building it is what
proves the layer is good. Keeping them in one repo lets the contract layer and
its biggest consumer evolve in lockstep, while the one-way dependency rule keeps
the API layer ignorant of the multiplayer mod.

## Thin authority: what the server does not do

The server does not simulate sailing. Wind, waves, hull physics, and rigging
stay on each client, because that simulation is the game and reproducing it
server-side would be a second game that disagrees with the first.

The server owns only what must be shared and durable:

- Presence: who is online and where.
- Moorage and persistent boats: boats that outlive a disconnect and a restart.
- A shared economy ledger: trades that affect a common price state.
- World clock and weather seed: one day/night cycle and one wind pattern for
  everyone.

## Non-goals

- The server is not a physics authority and will not become one.
- The mod does not redistribute any game code or asset. Game assemblies are
  referenced locally at build time and never shipped.
- The API layer does not promise to hide game updates. It promises to detect
  them and to convert a silent break into a visible failure.
