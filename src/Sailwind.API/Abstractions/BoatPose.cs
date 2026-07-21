using UnityEngine;

namespace Sailwind.Api
{
    /// <summary>Absolute world-space pose of a boat.</summary>
    public readonly struct BoatPose
    {
        public readonly Vector3 Position;
        public readonly Quaternion Rotation;
        public readonly Vector3 Velocity;

        public BoatPose(Vector3 position, Quaternion rotation, Vector3 velocity)
        {
            Position = position;
            Rotation = rotation;
            Velocity = velocity;
        }
    }
}
