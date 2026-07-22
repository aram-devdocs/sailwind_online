using System;
using BepInEx;
using HarmonyLib;
using Sailwind.Api.Adapters;
using Sailwind.Api.Runtime;

namespace Sailwind.Api
{
    /// <summary>
    /// Sailwind.API is itself a BepInEx plugin and the compatibility boundary other
    /// mods hard-depend on. On Awake it runs the surface check, wires the reader
    /// adapters, and arms the <see cref="SailwindApi.Ready"/> signal — all in a
    /// never-crash posture: drift degrades services instead of throwing.
    /// </summary>
    [BepInPlugin(Guid, Name, Version)]
    public sealed class SailwindApiPlugin : BaseUnityPlugin
    {
        public const string Guid = "com.aramdevdocs.sailwind.api";
        public const string Name = "Sailwind.API";
        public const string Version = "0.1.0";

        private readonly WorldReadyPoller _worldReady = new WorldReadyPoller();
        private Func<bool> _isWorldReady;
        private Action _signalReady;

        private void Awake()
        {
            var compat = SurfaceCheck.Run();
            SailwindApi.Compat = compat;
            SailwindApi.Version = Version;
            SailwindApi.SurfaceHash = compat.Hash;

            SailwindApi.Clock = new GameClockAdapter();
            SailwindApi.Wind = new WindAdapter();
            SailwindApi.PlayerBoat = new PlayerBoatAdapter();

            var saveEvents = new SaveEventsAdapter(new Harmony(Guid));
            SailwindApi.SaveEvents = saveEvents;
            _isWorldReady = () => saveEvents.IsWorldReady;
            _signalReady = SailwindApi.SignalReady;

            Logger.LogInfo(
                $"[Sailwind.API] Surface check {(compat.IsOk ? "OK" : "DRIFTED")} " +
                $"({Hash8(compat.Hash)}, {compat.MemberCount} members)");

            if (!compat.IsOk)
                foreach (var m in compat.Missing)
                    Logger.LogWarning($"[Sailwind.API] surface drift: {m}");
        }

        private void Update()
        {
            _worldReady.Poll(_isWorldReady, _signalReady);
        }

        private static string Hash8(string hash) =>
            string.IsNullOrEmpty(hash) ? "00000000" : hash.Substring(0, System.Math.Min(8, hash.Length));
    }
}
