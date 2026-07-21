using System;
using Sailwind.Api.Generated;
using Sailwind.Api.Runtime;

namespace Sailwind.Api.Adapters
{
    /// <summary>
    /// Reads day / time-of-day / moon phase off the game via the verified surface
    /// seam: <c>GameState.day</c> (static day counter), <c>Sun.localTime</c>
    /// (within-day time), and <c>Moon.currentPhase</c> (live lunar phase). Member
    /// names come from <see cref="GameRef"/> — resolved against the real assembly by
    /// codegen — so nothing here is a guessed magic string. Any unresolved member
    /// still degrades to a safe default rather than throwing.
    /// </summary>
    public sealed class GameClockAdapter : IGameClock
    {
        private readonly Type _gameStateType = GameBind.Resolve(GameRef.GameState);
        private readonly Type _sunType = GameBind.Resolve(GameRef.Sun);
        private readonly Type _moonType = GameBind.Resolve(GameRef.Moon);

        private readonly Func<object, object> _day;
        private readonly Func<object, object> _timeOfDay;
        private readonly Func<object, object> _moonPhase;

        private object _sun;
        private object _moon;

        public GameClockAdapter()
        {
            // GameState.day is static: no instance needed. Sun.localTime and
            // Moon.currentPhase are instance members on MonoBehaviours.
            _day = GameBind.Getter(_gameStateType, GameRef.GameState_day);
            _timeOfDay = GameBind.Getter(_sunType, GameRef.Sun_localTime);
            _moonPhase = GameBind.Getter(_moonType, GameRef.Moon_currentPhase);
        }

        private object Sun => _sun ??= GameBind.Instance(_sunType);
        private object Moon => _moon ??= GameBind.Instance(_moonType);

        public int Day => ReadInt(null, _day) ?? 0;
        public float TimeOfDay => ReadFloat(Sun, _timeOfDay) ?? 0f;
        public float MoonPhase => ReadFloat(Moon, _moonPhase) ?? 0f;

        // The getter closure ignores its argument for static fields, so a null
        // instance reads a static member; for an instance member a null instance
        // throws inside the getter and is caught here — degrade, never crash.
        private static float? ReadFloat(object instance, Func<object, object> getter)
        {
            if (getter == null) return null;
            try { return Convert.ToSingle(getter(instance)); }
            catch { return null; }
        }

        private static int? ReadInt(object instance, Func<object, object> getter)
        {
            if (getter == null) return null;
            try { return Convert.ToInt32(getter(instance)); }
            catch { return null; }
        }
    }
}
