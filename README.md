# Sailwind Online

Persistent shared seas for [Sailwind](https://store.steampowered.com/app/1764530/Sailwind/).
Sail alongside other players in one living world: pass other sailors on the open
ocean, moor your boat in a harbor that stays full while you are away, and trade
into a shared economy. You keep your own save and your own game; you are sharing
the world and the map with everyone else. You just join.

> **Status: early access, in active development.** The pieces below describe what
> exists today and what is coming. This is not yet on Thunderstore; for now you
> build it from source (see *Play* and *Host a server*).

## What it is

Two things ship from this repository:

- **Sailwind Online** — the multiplayer mod. A client plugin runs inside your
  game and talks to a lightweight server that owns the shared parts of the world:
  who is nearby, where boats are moored, the shared economy, and one clock and
  sky for everyone. Your sailing, cooking, and survival stay on your own machine,
  so the game feels exactly like Sailwind, just no longer empty.
- **Sailwind.API** — a mod-maker's toolkit. It turns the game's internals into a
  stable, version-checked set of interfaces so other mods can integrate cleanly
  and keep working across game updates instead of breaking silently.

## Play

The intended experience is the one you already know: install through a mod
manager, launch the game modded, and you are in a shared world. Quick-join drops
you onto a server automatically, or you can set a home server that connects on
launch. Your character and economy travel with you.

Until the Thunderstore release, play from source:

```sh
git clone https://github.com/aram-devdocs/sailwind_online
cd sailwind_online
make setup          # finds your Steam copy of Sailwind and prepares a build
make test-profile   # a dedicated mod profile, isolated from your own mods
make deploy         # build the plugins into that test profile
make run-modded     # start a local server and launch the game modded
```

`make run-modded` launches into an isolated **SailwindOnline** mod profile, so
your normal modded game is never touched. To play against someone else's server,
point the client at their address in the mod's config.

## Host a server

The server is a single small binary with no dependencies. It is cheap to run and
built to stay responsive: it only tracks the shared state, and each client does
the heavy simulation for the boats around it.

```sh
make server-build
./server/target/release/sw-server --bind 0.0.0.0:38455 --db world.sqlite
```

Open UDP port **38455** on your host, and share the address. World state persists
in a single SQLite file that survives restarts. Tagged server builds for Windows
and Linux are published on the
[Releases page](https://github.com/aram-devdocs/sailwind_online/releases).

## For mod authors

`Sailwind.API` gives you interfaces and events over the game (the clock, the
wind, the player's boat, save load and write) instead of raw reflection into
`Assembly-CSharp.dll`. It carries a surface hash checked at startup, so a game
update turns into a clear, isolated warning rather than a crash. Build against it
so your mod and Sailwind Online can coexist and integrate.

### Write your own mod

Two starting points build against the public `Sailwind.API` surface and nothing
else, which is what keeps a mod working across game updates:

- `samples/Sailwind.Mod.Sample` is a worked example plugin. It hard-depends on the
  Sailwind.API host, waits for the `Ready` signal, then reads the clock, the wind,
  and the player's boat through the public interfaces. It is one short, commented
  file that shows the whole consumer contract.
- `templates/sailwind-mod` is a `dotnet new` template that scaffolds the same shape
  in one command:

```sh
dotnet new install ./templates/sailwind-mod   # or: make template
dotnet new sailwind-mod --name My.Cool.Mod
```

The scaffolded project is a BepInEx plugin (net472) that references the
`Sailwind.Api.Abstractions` DLL shipped by the Sailwind.API host plugin. Point the
two paths at the top of the generated `.csproj` at your install, run `dotnet build`,
and drop the result into your BepInEx `plugins` folder. Your mod never touches the
game assembly directly; it reads the game only through Sailwind.API.

## What works today

- The client plugin loads in-game and passes its startup compatibility check.
- The wire protocol is proven end to end: a real client and the Rust server
  complete a handshake, exchange position updates, run server-driven interest
  management, persist a moored boat and an economy balance across a server
  restart, and shrug off malformed traffic (7 of 7 conformance checks).
- Foundations for moorage, the shared economy, and the world clock are in place.

The roadmap toward a full living world (presence replication, persistent boats,
the shared economy, home servers, and mod compatibility) is tracked on the
[issues board](https://github.com/aram-devdocs/sailwind_online/issues), grouped
into milestones M0 through M8.

## For developers

Contributions run through `AGENTS.md` (the entry point for every contributor,
human or agent) and the `/work` skill, which drives one issue from open to a
green pull request through review gates. Branches are `main` (release) and `dev`
(integration, default); work happens on `feat/<issue>-<slug>` branches with
squash-merged PRs and Conventional-Commit titles.

`make validate` is the canonical gate and mirrors CI exactly: instruction-layer
and leak checks, warnings-as-errors builds, every test suite, the enforced
dependency architecture, and contract-drift. Game DLLs never enter git, a
package, or CI.

### Repository map

- `contracts/` — FlatBuffers `.fbs` schemas (the single source of truth for every client-server message) and the committed C# bindings.
- `packages/` — pure, game-free libraries (`api-abstractions`, `net`, `sync`) plus the game-coupled `api-adapters`.
- `apps/` — the two thin BepInEx plugins that compose the packages: `Sailwind.API` and `Sailwind.Online.Client`.
- `server/` — the Rust workspace: the `sw-server` binary and the `sw-net`, `sw-world`, `sw-econ`, `sw-persist`, `sw-contracts` crates.
- `tools/` — `Sailwind.ApiGen` (Cecil codegen) and `protocol-smoke` (the wire-conformance harness).
- `tests/` — game-free tests that run in CI, the Cecil architecture tests, and game-coupled surface tests.
- `.agents/` — the single source of truth for agent configuration: rules, skills (including `/work`), and durable run state. `.claude/` symlinks into it.

### Documentation

- `docs/01-product/vision.md` — what each product is, why the split exists, and what the server deliberately does not simulate.
- `docs/02-architecture/monorepo-structure.md` — the apps-versus-packages layout and the one-way dependency rules the build enforces.
- `docs/02-architecture/game-integration.md` — the Sailwind.API design: publicizer, Cecil manifest, drift contract test, and surface hash.
- `docs/02-architecture/networking.md` — LiteNetLib UDP, FlatBuffers messages, interest management, and the clock model.
- `docs/02-architecture/persistence.md` — the SQLite model and how it maps from game save concepts without parsing a save file.
- `docs/03-guides/setup-windows.md` — prerequisites, `make setup`, and the test profile.
- `docs/03-guides/workflow.md` — the branch, issue, and PR flow, the review gates, and the `/work` loop.
- `docs/04-reference/game-facts.md` — version-specific facts: Steam build, Unity and BepInEx versions, install paths, confirmed game types.
- `docs/04-reference/prior-art.md` — existing modding tools, the compatibility seed list, and the breakage that motivates surface hashing.
- `docs/RELEASING.md` — the release runbook.

## License

MIT. See [LICENSE](LICENSE). Game assemblies are referenced locally at build time
from a gitignored `lib/` directory and are never redistributed, packed, or
committed.
