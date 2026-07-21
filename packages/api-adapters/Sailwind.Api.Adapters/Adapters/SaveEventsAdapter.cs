using System;
using HarmonyLib;
using Sailwind.Api.Generated;
using Sailwind.Api.Runtime;

namespace Sailwind.Api.Adapters
{
    /// <summary>
    /// Raises <see cref="WorldLoaded"/> / <see cref="SaveCompleted"/> from Harmony
    /// postfixes on the verified <c>SaveLoadManager.LoadGame</c> / <c>SaveGame</c>
    /// methods (names from <see cref="GameRef"/>, confirmed against the assembly by
    /// codegen — not a guessed candidate list, so the surface hash now covers them).
    /// If a method does not resolve, no patch is applied and the events simply never
    /// fire — degraded, never crashing.
    /// </summary>
    public sealed class SaveEventsAdapter : ISaveEvents
    {
        public event Action WorldLoaded;
        public event Action SaveCompleted;

        private static SaveEventsAdapter _active;

        public bool Patched { get; }

        public SaveEventsAdapter(Harmony harmony)
        {
            _active = this;
            Patched = TryPatch(harmony);
        }

        private static bool TryPatch(Harmony harmony)
        {
            var type = GameBind.Resolve(GameRef.SaveLoadManager);
            if (type == null) return false;

            var load = GameBind.Method(type, GameRef.SaveLoadManager_LoadGame);
            var save = GameBind.Method(type, GameRef.SaveLoadManager_SaveGame);

            bool patched = false;
            if (load != null)
            {
                harmony.Patch(load, postfix: new HarmonyMethod(typeof(SaveEventsAdapter), nameof(OnLoad)));
                patched = true;
            }
            if (save != null)
            {
                harmony.Patch(save, postfix: new HarmonyMethod(typeof(SaveEventsAdapter), nameof(OnSave)));
                patched = true;
            }
            return patched;
        }

        // These run as Harmony postfixes inside the game's own LoadGame/SaveGame, so a
        // subscriber that throws would propagate straight back into the game method and
        // could abort a load or save. Swallow handler exceptions here: an event handler
        // must never be able to break the game's save/load lifecycle.
        private static void OnLoad()
        {
            try { _active?.WorldLoaded?.Invoke(); }
            catch { /* handler failure must not abort the game's load */ }
        }

        private static void OnSave()
        {
            try { _active?.SaveCompleted?.Invoke(); }
            catch { /* handler failure must not abort the game's save */ }
        }
    }
}
