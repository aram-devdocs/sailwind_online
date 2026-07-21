# Lessons learned

Format: one dated line per entry (`YYYY-MM-DD: fact`), technical facts only,
newest at the bottom of its section. Hard cap 150 lines; prune superseded
entries whenever you touch this file. This is a whiteboard, not a journal: no
narrative, no status, no PM notes. Add a line in the same PR that discovers the
fact.

2026-07-21: Game is Unity 2019.1.10f1 Mono; BepInEx log reports "Supports SRE: False", so avoid System.Reflection.Emit (DynamicMethod) in our code; use Mono.Cecil offline and plain reflection at runtime.
2026-07-21: The game save format is .NET BinaryFormatter, not JSON; the Rust server never parses saves, it models state natively (BinaryFormatter is C#-only and hostile).
2026-07-21: ReflectionOnlyLoadFrom fails on Assembly-CSharp (Unity dependency resolution); codegen introspects with Mono.Cecil instead, which reads private members without loading Unity.
2026-07-21: Thunderstore Mod Manager injects BepInEx per profile; there is no BepInEx directory in the game install root, so setup reads it from the profile path.
2026-07-21: LiteNetLib is pinned to 1.3.1 because 2.x dropped net472/netstandard2.0 targets; 1.3.1 is the Unity/Mono-compatible line (net471 + netstandard2.0).
2026-07-21: rusqlite is pinned to 0.32.1 (features = ["bundled"]) because 0.40 pulls libsqlite3-sys 0.38, which needs a newer rustc than the pinned 1.86.0.
2026-07-21: FlatBuffers is pinned to 25.2.10 in lockstep across flatc, the Google.FlatBuffers C# runtime, and the Rust flatbuffers crate; bumping one without the others breaks the wire format.
2026-07-21: AnchorImprovements 1.1.7 threw a Harmony "Could not find method for type Anchor and name Start" in the user's log; that breakage is the motivating case for the API surface hash and runtime drift check.
2026-07-21: The game has no bare `Boat` or `Mooring` type. Real types: boat components via BoatRefs (fields boatModel/masts/walkCol) and NPCBoatController; mooring via BoatMooringManager / MooringSet. Player-boat pose access is through methods (GetCurrentBoat/GetBoatPos/GetBoatRigidbody), still to be wired for IPlayerBoatReader.
2026-07-21: The net472 client needs an explicit reference to UnityEngine.TextRenderingModule (TextAnchor lives there); IMGUIModule alone is not enough for GUIStyle.alignment.
2026-07-21: BepInEx.AssemblyPublicizer.MSBuild 0.4.3 needs ExcludeAssets="runtime" (with PrivateAssets="all"); otherwise its own deps (AsmResolver.*, BepInEx.AssemblyPublicizer.*) copy into plugin output and would ship in the Thunderstore package.
2026-07-21: The API surface hash for game build 23880593 (mvid fa39e939-d8e9-4d04-9648-4e10452ea8a3) is 792c8b4c over 11 verified types; regenerate with `make codegen` after any game update and expect the hash to change.
2026-07-21: Commit symlinks (CLAUDE.md, .claude/{rules,skills}) as git objects: write the target as the file content (no newline), git add, then `git update-index --cacheinfo 120000,<hash>,<path>` as the LAST staging step. Keep repo `core.symlinks=false` on Windows without Developer Mode — then git treats the 120000 entries as clean regular-file stand-ins and `git add -A` never downgrades them. With core.symlinks=true they show as ` T` type-changes and get clobbered.
2026-07-21: check-conventions.sh validates symlinks via `git ls-files -s` mode (the index), not `test -L`, so it is correct whether or not the checkout materialized real symlinks.
