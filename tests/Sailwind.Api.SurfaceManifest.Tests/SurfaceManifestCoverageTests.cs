using System.IO;
using System.Linq;
using System.Text.Json;
using Xunit;

namespace Sailwind.Api.SurfaceManifest.Tests
{
    /// <summary>
    /// Coverage gate over <c>manifest/api-surface.json</c>: the specific game members
    /// the four v0 adapters (GameClock/Wind/PlayerBoat/SaveEvents) bind to MUST appear
    /// in the surface, because a member the adapter uses but the manifest omits has no
    /// drift check and degrades silently on the next game update — the AnchorImprovements
    /// "could not find method" trap the surface hash exists to turn into a failing test.
    ///
    /// The generated Cecil facts in SurfaceContract.g.cs can only verify members that
    /// ARE in the manifest; a member the adapter needs but nobody seeded produces no
    /// fact at all, so this coverage assertion is the guard for the missing case. It
    /// reads only the committed JSON, so it stays in the game-free CI tier.
    /// </summary>
    public sealed class SurfaceManifestCoverageTests
    {
        public static TheoryData<string, string, string> AdapterMembers()
        {
            var data = new TheoryData<string, string, string>();
            data.Add("GameState", "day", "field");             // IGameClock.Day
            data.Add("GameState", "currentBoat", "field");      // IPlayerBoatReader boat handle
            data.Add("Sun", "localTime", "field");              // IGameClock.TimeOfDay
            data.Add("Moon", "currentPhase", "field");          // IGameClock.MoonPhase
            data.Add("Wind", "currentWind", "field");           // IWindReader.Ambient
            data.Add("SaveLoadManager", "LoadGame", "method");  // ISaveEvents.WorldLoaded patch target
            data.Add("SaveLoadManager", "SaveGame", "method");  // ISaveEvents.SaveCompleted patch target
            data.Add("SaveLoadManager", "readyToSave", "field"); // common new-game/continue world-ready marker
            return data;
        }

        [Theory]
        [MemberData(nameof(AdapterMembers))]
        public void Manifest_Covers_AdapterMember(string type, string member, string kind)
        {
            using var doc = JsonDocument.Parse(
                File.ReadAllText(SurfaceManifestIntegrity.ManifestJsonPath));

            bool found = doc.RootElement.GetProperty("members").EnumerateArray().Any(e =>
                e.GetProperty("type").GetString() == type &&
                e.GetProperty("member").GetString() == member &&
                e.GetProperty("kind").GetString() == kind);

            Assert.True(found,
                $"surface manifest does not cover {type}.{member} ({kind}); the adapter " +
                "would bind an unverified name and degrade silently on the next game update");
        }

        [Fact]
        public void Plugin_SourceWiring_PollsCommonWorldReadyMarkerInsteadOfLoadOnlyEvent()
        {
            // The plugin is game-coupled and cannot be referenced by this game-free
            // suite. WorldReadyPollerTests cover behavior; this assertion covers only
            // the Unity composition-root wiring that is otherwise inaccessible here.
            var pluginPath = Path.Combine(
                SurfaceManifestIntegrity.RepoRoot(), "apps", "Sailwind.API", "Plugin.cs");
            var source = File.ReadAllText(pluginPath);

            Assert.Contains("WorldReadyPoller", source);
            Assert.Contains("saveEvents.IsWorldReady", source);
            Assert.DoesNotContain("WorldLoaded += SailwindApi.SignalReady", source);
        }
    }
}
