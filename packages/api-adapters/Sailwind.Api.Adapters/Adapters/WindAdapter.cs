using System;
using Sailwind.Api.Runtime;
using UnityEngine;

namespace Sailwind.Api.Adapters
{
    /// <summary>
    /// Reads the ambient wind vector off the game's Wind via reflection, degrading
    /// to <see cref="Vector3.zero"/> when the member is unresolved or not a Vector3.
    /// </summary>
    public sealed class WindAdapter : IWindReader
    {
        private readonly Type _windType = GameBind.Resolve("Wind");
        private readonly Func<object, object> _ambient;
        private object _wind;

        public WindAdapter()
        {
            _ambient = GameBind.Getter(_windType, "globalWind", "windDirection", "direction", "wind");
        }

        private object Wind => _wind ??= GameBind.Instance(_windType);

        // System.Numerics at the boundary: the game's Wind exposes a UnityEngine.Vector3,
        // converted here so IWindReader stays UnityEngine-free.
        public System.Numerics.Vector3 Ambient
        {
            get
            {
                var wind = Wind;
                if (wind == null || _ambient == null) return System.Numerics.Vector3.Zero;
                try
                {
                    return _ambient(wind) is Vector3 v
                        ? new System.Numerics.Vector3(v.x, v.y, v.z)
                        : System.Numerics.Vector3.Zero;
                }
                catch
                {
                    return System.Numerics.Vector3.Zero;
                }
            }
        }
    }
}
