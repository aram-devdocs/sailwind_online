# Game facts

This is the substitutable layer. It is the only place in `docs/` allowed to
carry version numbers, build ids, and absolute paths, because those churn with
every game update. When a game update lands, this file changes and the timeless
docs do not.

All facts below were confirmed by recon on the local install on 2026-07-21.

## Build identity

- Game: Sailwind
- Steam appid: 1764530
- Steam buildid: 23880593
- Engine: Unity 2019.1.10f1, Mono scripting backend, ".NET 4.x" scripting
  runtime (BCL surface roughly .NET Framework 4.7.x; the game ships the
  `netstandard.dll` 2.0 facade, so both net472 and netstandard2.0 assemblies
  load; netstandard2.1 does not, as it requires Unity 2021.2 or newer).
- Ocean system: Crest.

## Modding stack

- BepInEx: 5.4.23.5 (from BepInExPack 5.4.2305).
- Doorstop: 4.5.0.
- BepInEx is injected per Thunderstore mod profile, not into the game install
  root; there is no BepInEx directory in the game folder itself.

## Paths

- Managed assemblies:
  `C:\Program Files (x86)\Steam\steamapps\common\Sailwind\Sailwind_Data\Managed\`
  (contains `Assembly-CSharp.dll`, `Assembly-CSharp-firstpass.dll`, `Crest.dll`,
  and the `UnityEngine*.dll` set).
- Steam app manifest (for buildid provenance):
  `steamapps\appmanifest_1764530.acf`.
- Thunderstore Mod Manager profiles root:
  `%APPDATA%\Thunderstore Mod Manager\DataFolder\Sailwind\profiles\`. Setup
  reads the BepInEx core from the `Default` profile; deploy writes the built
  plugins into the sibling `SailwindOnline` test profile, never into `Default`.

## Confirmed game type names

These types were confirmed present in `Assembly-CSharp.dll` during recon and are
the seed set the API codegen manifest draws from:

- `Boat`, `BoatRefs`: player and NPC boat state and references.
- `SaveLoadManager`, `SaveSlots`, `SaveContainer`: save lifecycle and the
  serialized world container.
- `GameState`: global game state.
- `Sun`: day, time, and moon-phase source.
- `Wind`: ambient wind source.
- `IslandMarket`: port market state.
- `Mooring`, `Anchor`: mooring and anchoring.
- `PlayerGold`, `Currency`: economy balances and currency.

Exact member names within these types are discovered at implementation time by
inspecting the DLL with the codegen tool's dump mode, then recorded in the API
manifest. Do not assume member names from this list alone.

## Save format

The game persists through `SaveContainer`, serialized with .NET
`BinaryFormatter` (not JSON). Confirmed `SaveContainer` field concepts include
port demands, market supply, currency rates, wind and wave state, day and time
and moon phase, and NPC boat data.

BinaryFormatter is C#-only and tightly coupled to the game's internal type
layout. The Rust server therefore never parses game saves; it models the shared
concepts natively in SQLite. See `docs/02-architecture/persistence.md`.

## Runtime note

The BepInEx log reports `Supports SRE: False` on this runtime, so
`System.Reflection.Emit` (for example `DynamicMethod`) is unavailable to our
code. Offline introspection uses Mono.Cecil and runtime surface checks use plain
reflection; neither needs Reflection.Emit.
