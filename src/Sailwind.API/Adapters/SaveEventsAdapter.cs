using System;
using HarmonyLib;
using Sailwind.Api.Runtime;

namespace Sailwind.Api.Adapters
{
    /// <summary>
    /// Raises <see cref="WorldLoaded"/> / <see cref="SaveCompleted"/> from Harmony
    /// postfixes on SaveLoadManager. Method names are discovery seeds (superseded by
    /// GameRef after codegen); if none resolve, no patch is applied and the events
    /// simply never fire — degraded, never crashing.
    /// </summary>
    internal sealed class SaveEventsAdapter : ISaveEvents
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
            var type = GameBind.Resolve("SaveLoadManager");
            if (type == null) return false;

            var load = GameBind.Method(type, "LoadGame", "LoadWorld", "Load", "OnWorldLoaded");
            var save = GameBind.Method(type, "SaveGame", "SaveWorld", "Save", "OnSaveCompleted");

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

        private static void OnLoad() => _active?.WorldLoaded?.Invoke();
        private static void OnSave() => _active?.SaveCompleted?.Invoke();
    }
}
