# Prior art and compatibility

This is the substitutable layer. It records the existing modding ecosystem this
project coexists with and the compatibility targets, both of which carry
version numbers and change as the ecosystem moves.

## SailwindModdingHelper

- Version confirmed: 2.1.1.
- What it is: the existing community "modding helper" for Sailwind and the de
  facto prior art for a shared modding API. Many published mods depend on it.
- Our stance: coexistence, not replacement. Sailwind.API MUST NOT conflict with
  SailwindModdingHelper at load time, because breaking the incumbent helper
  would break the mods that depend on it, and a compatibility layer that
  fragments the ecosystem is worse than none. It is a compatibility target we
  test against, not a competitor we displace.

## The AnchorImprovements breakage

- Mod: AnchorImprovements, version 1.1.7.
- Symptom: a Harmony patch threw `Could not find method for type Anchor and name
  Start` in the user's log, because a game update moved the member the patch
  targeted and the patch had no earlier signal that it would fail.
- Why it matters here: this is the motivating case for the API surface hash and
  the runtime drift check. Under Sailwind.API, a moved member is caught by the
  contract test in our build and, at runtime, degrades the affected service
  instead of throwing a patch-time exception. The AnchorImprovements failure is
  exactly the class of break the surface hash exists to convert from a user
  crash into a visible, isolated signal.

## Compatibility seed list

The compatibility matrix is seeded from the most-installed Sailwind mods, so a
game update or an API change is checked against real consumers rather than only
our own code. The target size for the seed is roughly the top 25 mods by
install count.

Seed set (from recon of a real 25-mod Sailwind profile, BepInEx 5.4.23.5). These
are the versions observed together in one working install; each row is a
compatibility target, not a verified pass. An untested mod is absent from the
"checked" column, not assumed compatible.

| Mod | Version | Notes |
|-----|---------|-------|
| SailwindModdingHelper | 2.1.1 | Incumbent modding helper; coexistence target. |
| RadFixes | 1.3.3 | Fixes; candidate recommended dependency. |
| HooksHangMore | 2.2.1 | Gameplay tweak. |
| BetterFishing | 1.5.0 | Local gameplay; read-only expected. |
| BitsAndBobsRadRedux | 1.3.2 | Content. |
| InstrumentDisplay | 2.0.0 | HUD/info; client-local. |
| Climate | 1.0.1 | Weather; interacts with shared world clock/weather seed. |
| PlayerHome | 1.0.0 | Depends on SailwindModdingHelper. |
| RandomEncounters | 1.4.1 | World events; may conflict with a shared world. |
| ModSaveBackups | 1.2.0 | Save backups; save-adjacent. |
| RadRefinements | 1.5.3 | QoL. |
| TowableBoats | 0.2.3 | Physics/towing; relevant to boat ownership handoff. |
| Dinghies | 1.0.11 | Boat content. |
| PassageDude | 1.0.6 | Gameplay. |
| SimpleTides | 1.0.2 | Tides; interacts with shared world state. |
| SailInfo | 1.2.1 | Info HUD; client-local. |
| Sail_a_dex | 1.8.0 | Info. |
| CookedInfo | 1.2.3 | Info. |
| ProfitPercent | 1.2.1 | Economy display; interacts with shared economy. |
| AnchorImprovements | 1.1.7 | Harmony breakage on a moved member; surface-hash motivating case. |
| FurnitureFix | 1.1.3 | Fix. |
| ModVersionChecker | 1.2.2 | Version gating; pattern reference for our compat warnings. |
| EconomicEvents | 1.3.1 | Economy events; conflicts with a shared authoritative economy. |
| Water_Push_Restored | 1.0.2 | Physics. |
| BitsAndBobsRadRedux / configurationmanager / stickyfix | v19.0 (cfgmgr) | ConfigurationManager and StickyFix present in the same profile. |

Categories to decide per the compatibility strategy: info/HUD mods (SailInfo,
InstrumentDisplay, Sail_a_dex, CookedInfo) are passive client-local; economy and
world-event mods (EconomicEvents, RandomEncounters, ProfitPercent) contend with
the shared authoritative economy and world; physics mods (TowableBoats,
Water_Push_Restored, SimpleTides, Climate) contend with ownership handoff and the
shared clock/weather seed. Each is populated with a checked result as it is
tested against Sailwind.API and Sailwind.Online.
