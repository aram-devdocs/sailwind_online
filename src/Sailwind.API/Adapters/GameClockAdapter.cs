using System;
using Sailwind.Api.Runtime;

namespace Sailwind.Api.Adapters
{
    /// <summary>
    /// Reads day / time-of-day / moon phase off the game's Sun (and GameState for
    /// the day counter) via reflection. Any unresolved member degrades to a safe
    /// default rather than throwing. Candidate names are discovery seeds superseded
    /// by GameRef after codegen.
    /// </summary>
    internal sealed class GameClockAdapter : IGameClock
    {
        private readonly Type _sunType = GameBind.Resolve("Sun");
        private readonly Type _gameStateType = GameBind.Resolve("GameState");

        private readonly Func<object, object> _timeOfDay;
        private readonly Func<object, object> _moonPhase;
        private readonly Func<object, object> _sunDay;
        private readonly Func<object, object> _gameStateDay;

        private object _sun;
        private object _gameState;

        public GameClockAdapter()
        {
            _timeOfDay = GameBind.Getter(_sunType, "timeOfDay", "currentTime", "time");
            _moonPhase = GameBind.Getter(_sunType, "moonPhase", "moon", "lunarPhase");
            _sunDay = GameBind.Getter(_sunType, "day", "currentDay", "dayNumber");
            _gameStateDay = GameBind.Getter(_gameStateType, "day", "currentDay", "dayNumber");
        }

        private object Sun => _sun ??= GameBind.Instance(_sunType);
        private object GameState => _gameState ??= GameBind.Instance(_gameStateType);

        public int Day => ReadInt(GameState, _gameStateDay) ?? ReadInt(Sun, _sunDay) ?? 0;
        public float TimeOfDay => ReadFloat(Sun, _timeOfDay) ?? 0f;
        public float MoonPhase => ReadFloat(Sun, _moonPhase) ?? 0f;

        private static float? ReadFloat(object instance, Func<object, object> getter)
        {
            if (instance == null || getter == null) return null;
            try { return Convert.ToSingle(getter(instance)); }
            catch { return null; }
        }

        private static int? ReadInt(object instance, Func<object, object> getter)
        {
            if (instance == null || getter == null) return null;
            try { return Convert.ToInt32(getter(instance)); }
            catch { return null; }
        }
    }
}
