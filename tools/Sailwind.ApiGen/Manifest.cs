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
    /// v0 manifest seed. Intentionally small and conservative: it names only the
    /// recon-confirmed game <em>types</em> the init-0 adapters reach into. Exact
    /// member names are discovered against the real DLL with
    /// <c>--dump-type &lt;name&gt;</c> and added here; the collect-all-drift verify
    /// pass then turns any wrong name into a single, actionable failure rather
    /// than a runtime crash. Keeping the seed to verified facts is deliberate:
    /// a fabricated member name would ship a green build that breaks in-game.
    /// </summary>
    public static class Seed
    {
        public static readonly IReadOnlyList<GameMember> Members = new List<GameMember>
        {
            new("GameState",       "", Kinds.Type, Note: "root game controller / world state"),
            new("Sun",             "", Kinds.Type, Note: "day + time-of-day + moon phase source"),
            new("Wind",            "", Kinds.Type, Note: "ambient wind vector source"),
            new("Boat",            "", Kinds.Type, Note: "boat entity (player + NPC)"),
            new("BoatRefs",        "", Kinds.Type, Note: "cached component refs on a Boat"),
            new("SaveLoadManager", "", Kinds.Type, Note: "save/load lifecycle for save events"),
            new("SaveSlots",       "", Kinds.Type, Note: "save-slot enumeration"),
            new("Mooring",         "", Kinds.Type, Note: "moorage anchor point"),
            new("PlayerGold",      "", Kinds.Type, Note: "player wallet"),
            new("Currency",        "", Kinds.Type, Note: "currency amount value type"),
            new("IslandMarket",    "", Kinds.Type, Note: "island trading market"),
        };
    }
}
