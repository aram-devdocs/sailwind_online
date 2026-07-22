using BepInEx;
using Sailwind.Api;

namespace SailwindModTemplate
{
    /// <summary>
    /// Your Sailwind mod. It is a BepInEx plugin that consumes ONLY the public
    /// Sailwind.API surface: the <see cref="SailwindApi"/> facade and its reader
    /// interfaces. It never touches the game assembly directly — the API is your
    /// stable, version-checked window onto the game.
    /// </summary>
    [BepInPlugin(PluginGuid, PluginName, PluginVersion)]
    // Hard-depend on the Sailwind.API host plugin so BepInEx loads it first: SailwindApi
    // is armed before this Awake runs. This is how a mod couples to the API — through a
    // BepInEx dependency on the host, not a project reference to it.
    [BepInDependency(SailwindApiGuid, BepInDependency.DependencyFlags.HardDependency)]
    public sealed class Plugin : BaseUnityPlugin
    {
        public const string PluginGuid = "REPLACE_GUID_PREFIX.SailwindModTemplate";
        public const string PluginName = "SailwindModTemplate";
        public const string PluginVersion = "1.0.0";

        // The Sailwind.API host plugin's GUID. Must match the host's [BepInPlugin] GUID.
        private const string SailwindApiGuid = "com.aramdevdocs.sailwind.api";

        private void Awake()
        {
            // SailwindApi.Ready fires once the world is loaded AND the API surface is verified.
            // Subscribing after it has already fired invokes the handler immediately, so this is
            // safe regardless of plugin load order.
            SailwindApi.Ready += OnSailwindReady;

            Logger.LogInfo(PluginName + " loaded; waiting for Sailwind.API.");
        }

        private void OnSailwindReady()
        {
            Logger.LogInfo("Sailwind.API " + SailwindApi.Version + " ready (surface " + SailwindApi.SurfaceHash + ").");

            // Read the shared world clock: day number, time-of-day, and moon phase.
            IGameClock clock = SailwindApi.Clock;
            if (clock != null)
            {
                Logger.LogInfo("Clock: day " + clock.Day + ", timeOfDay " + clock.TimeOfDay + ", moon " + clock.MoonPhase + ".");
            }

            // Read the ambient wind as a UnityEngine-free System.Numerics vector.
            IWindReader wind = SailwindApi.Wind;
            if (wind != null)
            {
                Logger.LogInfo("Wind: " + wind.Ambient + ".");
            }

            // Read the local player's boat pose, when the player is aboard one.
            IPlayerBoatReader playerBoat = SailwindApi.PlayerBoat;
            if (playerBoat != null && playerBoat.HasBoat)
            {
                BoatPose pose = playerBoat.Pose;
                Logger.LogInfo("Player boat at " + pose.Position + " moving " + pose.Velocity + ".");
            }

            // The save lifecycle keeps firing after Ready; react to each save write here.
            ISaveEvents saveEvents = SailwindApi.SaveEvents;
            if (saveEvents != null)
            {
                saveEvents.SaveCompleted += OnSaveCompleted;
            }
        }

        private void OnSaveCompleted()
        {
            Logger.LogInfo("Game saved; persist your own mod state alongside here.");
        }

        private void OnDestroy()
        {
            // Always unsubscribe: the API facade is static and outlives this plugin instance.
            SailwindApi.Ready -= OnSailwindReady;

            ISaveEvents saveEvents = SailwindApi.SaveEvents;
            if (saveEvents != null)
            {
                saveEvents.SaveCompleted -= OnSaveCompleted;
            }
        }
    }
}
