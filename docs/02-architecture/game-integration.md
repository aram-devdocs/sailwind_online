# Game integration

Sailwind.API is a generated seam over the game's compiled assembly. The design
goal is one property: a game update that moves the ground under us fails a test
in our build, not a user's install at runtime.

## The problem

Mods reach into a game by referencing its compiled assembly and calling its
types. Those types are internal, private, and unstable. The common approach is
reflection with string member names, which compiles fine and then throws at
runtime the first time a game update renames a method. The user sees a crash;
the author finds out from a bug report.

## The approach

Four moving parts turn that runtime surprise into a build-time signal.

### Publicized reference assemblies

At build time an MSBuild task produces a publicized copy of the game assembly,
where private and internal members become accessible for compilation only. This
lets the API code call game internals directly, with real compiler type
checking, instead of through reflection. The publicized assembly exists in the
build output only and is never redistributed, because it is derived from game
IP.

Publicizing removes the compiler as a tripwire (publicized access papers over a
renamed member until runtime), so the drift machinery below is what restores
the alarm.

### A declarative member manifest

Every game member the API touches is listed in one manifest: its type, its
name, its kind (type, field, method, property), and whether it is static. The
manifest is the explicit contract of exactly how much game surface we depend on.
Nothing reaches a game member without an entry here.

### Cecil introspection and drift detection

A tool reads the raw (non-publicized) game assembly with Mono.Cecil, which
enumerates private members without loading the Unity dependency graph. It checks
every manifest entry against the real assembly and collects the full list of
mismatches before failing once, so one run reports every drift a game update
caused rather than stopping at the first. Reflection-only loading was rejected:
it fails on the game assembly because Unity dependencies do not resolve.

### The surface hash

The verified surface (types, members, signatures) is hashed into a single
token. That hash is embedded in the API assembly and checked at plugin startup
against the live game assembly using plain reflection. A matching hash means the
surface is intact and the mod runs. A mismatch puts the affected services into a
degraded mode that reports themselves unavailable, rather than throwing a
patch-time exception that takes down unrelated mods.

## Generated output

The tool emits committed source: verified member-name constants (so patches
carry no magic strings), the embedded manifest and hash, and a contract test
with one assertion per manifest entry. The contract test runs wherever the game
assembly is present and is skipped cleanly where it is not, so cloud CI stays
green without game IP.

## The tradeoff we accepted

Publicizing is a deliberate choice. It buys direct, type-checked access to game
internals at the cost of the compiler no longer catching a moved member. We took
that trade because Sailwind.API is itself the compatibility boundary other mods
depend on, and a typed API plus a drift contract is a better boundary than a
reflection seam. The drift check and surface hash are the price that keeps the
trade honest.
