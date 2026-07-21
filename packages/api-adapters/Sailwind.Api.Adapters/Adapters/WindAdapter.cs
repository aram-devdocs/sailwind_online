using System;
using Sailwind.Api.Generated;
using Sailwind.Api.Runtime;
using UnityEngine;

namespace Sailwind.Api.Adapters
{
    /// <summary>
    /// Reads the ambient wind vector off the game's <c>Wind.currentWind</c> (a static
    /// <see cref="Vector3"/>, the field the game reads everywhere) via the verified
    /// surface seam, degrading to <see cref="Vector3.zero"/> when the member is
    /// unresolved or not a Vector3. The member name comes from <see cref="GameRef"/>,
    /// not a guessed candidate list.
    /// </summary>
    public sealed class WindAdapter : IWindReader
    {
        private readonly Type _windType = GameBind.Resolve(GameRef.Wind);
        private readonly Func<object, object> _ambient;

        public WindAdapter()
        {
            _ambient = GameBind.Getter(_windType, GameRef.Wind_currentWind);
        }

        // System.Numerics at the boundary: the game's Wind exposes a UnityEngine.Vector3,
        // converted here so IWindReader stays UnityEngine-free.
        public System.Numerics.Vector3 Ambient
        {
            get
            {
                if (_ambient == null) return System.Numerics.Vector3.Zero;
                try
                {
                    // currentWind is static, so the getter ignores its argument.
                    return _ambient(null) is Vector3 v
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
