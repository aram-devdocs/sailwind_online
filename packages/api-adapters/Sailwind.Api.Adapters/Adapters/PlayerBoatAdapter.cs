using System;
using Sailwind.Api.Generated;
using Sailwind.Api.Runtime;
using UnityEngine;

namespace Sailwind.Api.Adapters
{
    /// <summary>
    /// Exposes the local player's boat pose. The player's boat is located through the
    /// verified surface seam <c>GameState.currentBoat</c> — a static
    /// <see cref="Transform"/> the game keeps pointed at the boat the player is aboard.
    /// The pose itself is exact once the Transform resolves (Unity Transform +
    /// Rigidbody). When no boat is boarded the field is null, <see cref="HasBoat"/> is
    /// false, and the service is simply unavailable.
    /// </summary>
    public sealed class PlayerBoatAdapter : IPlayerBoatReader
    {
        private readonly Type _gameStateType = GameBind.Resolve(GameRef.GameState);
        private readonly Func<object, object> _currentBoat;

        public PlayerBoatAdapter()
        {
            _currentBoat = GameBind.Getter(_gameStateType, GameRef.GameState_currentBoat);
        }

        // GameState.currentBoat is static, so the getter ignores its argument.
        private Transform PlayerBoat
        {
            get
            {
                if (_currentBoat == null) return null;
                try { return _currentBoat(null) as Transform; }
                catch { return null; }
            }
        }

        public bool HasBoat => PlayerBoat != null;

        public BoatPose Pose
        {
            get
            {
                var tf = PlayerBoat;
                if (tf == null) return default;
                try
                {
                    var body = tf.GetComponent<Rigidbody>();
                    var velocity = body != null ? body.velocity : Vector3.zero;
                    // Convert UnityEngine.Vector3/Quaternion -> System.Numerics at the boundary
                    // so BoatPose (and everything downstream) stays UnityEngine-free.
                    return new BoatPose(ToNumerics(tf.position), ToNumerics(tf.rotation), ToNumerics(velocity));
                }
                catch
                {
                    return default;
                }
            }
        }

        private static System.Numerics.Vector3 ToNumerics(Vector3 v) =>
            new System.Numerics.Vector3(v.x, v.y, v.z);

        private static System.Numerics.Quaternion ToNumerics(Quaternion q) =>
            new System.Numerics.Quaternion(q.x, q.y, q.z, q.w);
    }
}
