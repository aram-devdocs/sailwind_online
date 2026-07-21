# Sailwind Online

Two products for [Sailwind](https://store.steampowered.com/app/1764530/Sailwind/),
one repository. **Sailwind.API** is a code-generated contract layer that other
mods build on: it turns the game's internals into stable, version-checked
interfaces and events. **Sailwind.Online** is a persistent-world multiplayer
mod: a BepInEx 5 client plugin talks to a standalone Rust server that holds thin
authority over presence, moorage, a shared economy ledger, and the world clock.
Heavy sailing simulation stays on the clients; the server keeps everyone in the
same world.

## Products

| Product | What it is | Who it is for | Status |
|---------|-----------|---------------|--------|
| Sailwind.API | A generated contract layer over `Assembly-CSharp.dll`. Publicized reference assemblies at build time, a Mono.Cecil manifest of every game member used, and a surface hash that fails loudly when a game update breaks it. | Mod authors who want a stable target instead of raw reflection. | Init-0: codegen and surface machinery in progress. |
| Sailwind.Online | A BepInEx 5 client plus a Rust server. LiteNetLib UDP transport, FlatBuffers messages, SQLite persistence, server-driven interest management. | Players who want shared seas: see other sailors, moor in a living harbor, trade in one economy. | Init-0: transport and handshake in progress. |

## Quickstart

Windows first. A POSIX shell (Git Bash) covers the cross-platform scripts; the
`make` targets pick the right script mirror per platform.

```sh
git clone https://github.com/aram-devdocs/sailwind_online
cd sailwind_online

make setup   # finds your Steam install, populates gitignored lib/, builds .sandbox/
make check   # fast gate: game-free build, tests, and Rust checks
```

`make setup` needs a local Steam copy of Sailwind. Without one it stops with a
clear message, and the game-free tier still builds. Game DLLs never enter git,
a package, or CI. Run `make` with no target to list every task.

## Repository map

- `contracts/` FlatBuffers `.fbs` schemas (the single source of truth for every client-server message) plus the committed C# bindings under `contracts/cs/`.
- `packages/` the pure, game-free netstandard2.0 libraries (`api-abstractions`, `net`, `sync`) plus the game-coupled net472 `api-adapters`.
- `apps/` the two thin net472 BepInEx plugins that compose the packages: `Sailwind.API` and `Sailwind.Online.Client`.
- `server/` the Rust workspace: the `sw-server` binary and the `sw-net`, `sw-world`, `sw-econ`, `sw-persist`, and `sw-contracts` crates.
- `tools/` net8 helpers: `Sailwind.ApiGen` (Cecil introspection codegen) and `protocol-smoke` (the wire-conformance harness).
- `tests/` game-free test projects that run in CI, plus game-coupled surface tests that run locally.
- `build/` shared MSBuild props and targets (game references, packaging, deploy, versions).
- `scripts/` `.ps1` scripts with `.sh` mirrors, invoked identically by `make` and by CI.
- `docs/` the documentation tree, indexed below.
- `.claude/` committed agent configuration: rules, hooks, and skills.

## How work happens

Two branches: `main` is release-only, `dev` is the integration branch and the
GitHub default. There is no `master`. Work happens on `feat/<issue>-<slug>`
branches cut from `dev`, and every change lands through a squash-merged pull
request back to `dev`. PR titles are Conventional Commits, because the squash
title becomes the commit that release tooling reads.

The backlog lives on the
[GitHub issues board](https://github.com/aram-devdocs/sailwind_online/issues),
grouped by milestones M0 through M8. `AGENTS.md` is the entry point for every
contributor, human or agent, and the `/work` skill drives one issue from open to
a green PR. Read `AGENTS.md` before you start.

## Documentation

- `docs/01-product/vision.md` what each product is, why the split exists, and what the server deliberately does not simulate.
- `docs/02-architecture/monorepo-structure.md` the apps-versus-packages layout, language grouping, and one-way dependency rules.
- `docs/02-architecture/game-integration.md` the Sailwind.API design: publicizer, Cecil manifest, drift contract test, and surface hash.
- `docs/02-architecture/networking.md` LiteNetLib UDP, FlatBuffers messages, interest management, and the tick and clock model.
- `docs/02-architecture/persistence.md` the Rust-side SQLite model and how it maps from game save concepts without ever parsing a save file.
- `docs/03-guides/setup-windows.md` prerequisites, `make setup`, the sandbox layout, and how deploy-to-sandbox works.
- `docs/03-guides/workflow.md` the human-facing branch, issue, and PR flow, and how merging works.
- `docs/04-reference/game-facts.md` version-specific facts: Steam build, Unity and BepInEx versions, install paths, and confirmed game types.
- `docs/04-reference/prior-art.md` existing modding tools, the compatibility seed list, and the breakage that motivates surface hashing.
- `docs/RELEASING.md` the release runbook: tags, secrets, the compat matrix, and the Thunderstore packaging flow.

## License

MIT. See [LICENSE](LICENSE). Game assemblies are referenced locally at build
time from a gitignored `lib/` directory and are never redistributed, packed, or
committed.
