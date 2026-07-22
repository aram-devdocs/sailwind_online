using System.Collections.Generic;

namespace Sailwind.ApiGen
{
    /// <summary>
    /// One declared dependency on a member of the game assembly. This is the
    /// hand-maintained source of truth that ApiGen verifies against the real
    /// <c>Assembly-CSharp.dll</c> and then projects into generated code + the
    /// canonical surface manifest.
    /// </summary>
    /// <param name="Type">Simple type name as it appears in the game assembly.</param>
    /// <param name="Member">
    /// Member name. For <see cref="Kinds.Type"/> entries this is the empty string
    /// (the type itself is the subject).
    /// </param>
    /// <param name="Kind">One of the <see cref="Kinds"/> values.</param>
    /// <param name="Static">Expected static-ness of the member.</param>
    /// <param name="Note">Free-form provenance / discovery note.</param>
    public sealed record GameMember(
        string Type,
        string Member,
        string Kind,
        bool Static = false,
        string Note = null);

    public static class Kinds
    {
        public const string Type = "type";
        public const string Field = "field";
        public const string Method = "method";
        public const string Property = "property";
    }

    /// <summary>
    /// v0 manifest seed. Every entry is a recon-confirmed fact about the real
    /// <c>Assembly-CSharp.dll</c>: the game <em>types</em> the init-0 adapters reach
    /// into, plus the exact <em>members</em> the four v0 adapters bind to (day /
    /// time-of-day / moon phase, ambient wind, player-boat handle, save+load
    /// lifecycle). Member names are discovered against the DLL with
    /// <c>--dump-type &lt;name&gt;</c> and confirmed here; the collect-all-drift verify
    /// pass then turns any wrong name into a single, actionable failure rather
    /// than a runtime crash. Keeping the seed to verified facts is deliberate:
    /// a fabricated member name would ship a green build that breaks in-game, and
    /// a member the adapter uses but nobody seeded degrades silently on the next
    /// game update (the AnchorImprovements "could not find method" trap).
    /// </summary>
    public static class Seed
    {
        public static readonly IReadOnlyList<GameMember> Members = new List<GameMember>
        {
            // --- Types (root handles + reserved surface for later milestones) ---
            new("GameState",       "", Kinds.Type, Note: "root game controller / world state"),
            new("Sun",             "", Kinds.Type, Note: "time-of-day source (MonoBehaviour)"),
            new("Moon",            "", Kinds.Type, Note: "moon-phase source (MonoBehaviour)"),
            new("Wind",            "", Kinds.Type, Note: "ambient wind vector source"),
            new("BoatRefs",          "", Kinds.Type, Note: "cached component refs on a boat (boatModel Transform, masts, walkCol)"),
            new("NPCBoatController",   "", Kinds.Type, Note: "AI boat controller; basis for remote/NPC boat modelling (M5)"),
            new("SaveLoadManager",   "", Kinds.Type, Note: "save/load lifecycle for save events"),
            new("SaveSlots",         "", Kinds.Type, Note: "save-slot enumeration"),
            new("BoatMooringManager", "", Kinds.Type, Note: "mooring/dock manager; basis for moorage modelling (M6)"),
            new("PlayerGold",      "", Kinds.Type, Note: "player wallet"),
            new("Currency",        "", Kinds.Type, Note: "currency amount value type"),
            new("IslandMarket",    "", Kinds.Type, Note: "island trading market"),

            // --- Members the four v0 adapters bind to (verified against lib/) ---
            // IGameClock (GameClockAdapter)
            new("GameState", "day",       Kinds.Field,  Static: true,  Note: "IGameClock.Day: static int day counter"),
            new("Sun",       "localTime", Kinds.Field,  Static: false, Note: "IGameClock.TimeOfDay: within-day time (drives dawn/night lerp), derived from globalTime"),
            new("Moon",      "currentPhase", Kinds.Field, Static: false, Note: "IGameClock.MoonPhase: live moon phase on Moon (Moon.instance)"),
            // IWindReader (WindAdapter)
            new("Wind",      "currentWind", Kinds.Field, Static: true,  Note: "IWindReader.Ambient: static current wind vector (the widely-read wind)"),
            // IPlayerBoatReader (PlayerBoatAdapter)
            new("GameState", "currentBoat", Kinds.Field, Static: true,  Note: "player-boat handle: static Transform of the player's current boat"),
            // ISaveEvents (SaveEventsAdapter) — common readiness + Harmony postfix targets
            new("SaveLoadManager", "readyToSave", Kinds.Field, Static: true, Note: "common world-ready marker set by both new-game and continue flows"),
            new("SaveLoadManager", "LoadGame", Kinds.Method, Static: false, Note: "ISaveEvents.WorldLoaded postfix target (closes the #5 unverified-method trap)"),
            new("SaveLoadManager", "SaveGame", Kinds.Method, Static: false, Note: "ISaveEvents.SaveCompleted postfix target"),
        };
    }
}
