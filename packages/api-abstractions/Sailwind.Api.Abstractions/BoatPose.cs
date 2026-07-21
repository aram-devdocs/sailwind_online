using System.Numerics;

namespace Sailwind.Api
{
    /// <summary>
    /// Absolute world-space pose of a boat. Uses <see cref="System.Numerics"/> types so
    /// this contract carries no UnityEngine dependency; the game adapter converts from
    /// UnityEngine.Vector3/Quaternion at the boundary.
    /// </summary>
    public readonly struct BoatPose
    {
        public Vector3 Position { get; }
        public Quaternion Rotation { get; }
        public Vector3 Velocity { get; }

        public BoatPose(Vector3 position, Quaternion rotation, Vector3 velocity)
        {
            Position = position;
            Rotation = rotation;
            Velocity = velocity;
        }
    }
}
