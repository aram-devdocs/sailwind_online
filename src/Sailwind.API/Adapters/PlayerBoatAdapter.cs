using System;
using Sailwind.Api.Runtime;
using UnityEngine;

namespace Sailwind.Api.Adapters
{
    /// <summary>
    /// Exposes the local player's boat pose. The pose itself comes from Unity
    /// (Transform + Rigidbody) so it is exact once the boat instance is resolved;
    /// locating the <em>player's</em> boat is the discovery item, attempted via a
    /// static accessor on Boat / BoatRefs. When it cannot be resolved,
    /// <see cref="HasBoat"/> is false and the service is simply unavailable.
    /// </summary>
    internal sealed class PlayerBoatAdapter : IPlayerBoatReader
    {
        private readonly Func<object, object> _playerBoat;

        public PlayerBoatAdapter()
        {
            _playerBoat =
                GameBind.Getter(GameBind.Resolve("Boat"), "playerBoat", "localBoat", "mainBoat")
                ?? GameBind.Getter(GameBind.Resolve("BoatRefs"), "playerBoat", "localBoat");
        }

        private Component PlayerBoat
        {
            get
            {
                if (_playerBoat == null) return null;
                try { return _playerBoat(null) as Component; }
                catch { return null; }
            }
        }

        public bool HasBoat => PlayerBoat != null;

        public BoatPose Pose
        {
            get
            {
                var boat = PlayerBoat;
                if (boat == null) return default;
                try
                {
                    var tf = boat.transform;
                    var body = boat.GetComponent<Rigidbody>();
                    var velocity = body != null ? body.velocity : Vector3.zero;
                    return new BoatPose(tf.position, tf.rotation, velocity);
                }
                catch
                {
                    return default;
                }
            }
        }
    }
}
