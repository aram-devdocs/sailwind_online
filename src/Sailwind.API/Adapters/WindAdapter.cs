using System;
using Sailwind.Api.Runtime;
using UnityEngine;

namespace Sailwind.Api.Adapters
{
    /// <summary>
    /// Reads the ambient wind vector off the game's Wind via reflection, degrading
    /// to <see cref="Vector3.zero"/> when the member is unresolved or not a Vector3.
    /// </summary>
    internal sealed class WindAdapter : IWindReader
    {
        private readonly Type _windType = GameBind.Resolve("Wind");
        private readonly Func<object, object> _ambient;
        private object _wind;

        public WindAdapter()
        {
            _ambient = GameBind.Getter(_windType, "globalWind", "windDirection", "direction", "wind");
        }

        private object Wind => _wind ??= GameBind.Instance(_windType);

        public Vector3 Ambient
        {
            get
            {
                var wind = Wind;
                if (wind == null || _ambient == null) return Vector3.zero;
                try
                {
                    return _ambient(wind) is Vector3 v ? v : Vector3.zero;
                }
                catch
                {
                    return Vector3.zero;
                }
            }
        }
    }
}
