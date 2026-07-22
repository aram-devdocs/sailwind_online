using System.Numerics;

namespace Sailwind.Online.Client.Sync
{
    /// <summary>A smoothed position/rotation produced by the snapshot sampler at a render time.</summary>
    public readonly struct SampledPose
    {
        /// <summary>Interpolated (or extrapolated) world position.</summary>
        public NetVec3 Pos { get; }

        /// <summary>Interpolated world rotation (extrapolation holds the latest orientation).</summary>
        public NetQuat Rot { get; }

        public SampledPose(NetVec3 pos, NetQuat rot)
        {
            Pos = pos;
            Rot = rot;
        }
    }

    /// <summary>
    /// Snapshot interpolation/extrapolation sampler. Turns the timestamped ring buffer of an
    /// <see cref="EntityTrack"/> into a single smoothed pose at a given render time, so a remote
    /// entity moves fluidly between the discrete snapshots the server sends.
    ///
    /// The render time and each sample's <see cref="EntitySample.TMs"/> share the wire-authored
    /// (ms) timeline. Position is LERPed and rotation SLERPed between the two snapshots bracketing
    /// (renderTime - interpolationDelay); the delay renders slightly in the past so there is
    /// normally a future sample to interpolate toward. When the render time runs past the newest
    /// snapshot the pose is extrapolated from the newest linear velocity (units/second), capped so
    /// a stale entity cannot fly away.
    ///
    /// Pure C# on <see cref="System.Numerics"/> math, no UnityEngine dependency, so it is
    /// unit-testable in a game-free process.
    /// </summary>
    public static class Interpolator
    {
        /// <summary>Render-behind delay (ms): how far into the past the sampler renders by default.</summary>
        public const long DefaultInterpolationDelayMs = 100;

        /// <summary>Maximum extrapolation window (ms) past the newest snapshot before the pose is frozen.</summary>
        public const long DefaultMaxExtrapolationMs = 250;

        /// <summary>
        /// Sample a smoothed pose for <paramref name="track"/> at <paramref name="renderTimeMs"/>.
        /// Returns <c>null</c> when the track holds no samples.
        /// </summary>
        public static SampledPose? SampleAt(
            EntityTrack track,
            long renderTimeMs,
            long interpolationDelayMs = DefaultInterpolationDelayMs,
            long maxExtrapolationMs = DefaultMaxExtrapolationMs)
        {
            int count = track.SampleCount;
            if (count == 0)
            {
                return null;
            }

            if (count == 1)
            {
                track.TryGetSample(0, out EntitySample only);
                return new SampledPose(only.Pos, only.Rot);
            }

            long effectiveMs = renderTimeMs - interpolationDelayMs;

            track.TryGetSample(0, out EntitySample oldest);
            track.TryGetSample(count - 1, out EntitySample newest);

            // Before the oldest retained sample: clamp to the oldest rather than guess a past pose.
            if (effectiveMs <= oldest.TMs)
            {
                return new SampledPose(oldest.Pos, oldest.Rot);
            }

            // Past the newest sample: extrapolate along the latest velocity, capped so a stale
            // entity does not fly off. Rotation holds the latest orientation.
            if (effectiveMs >= newest.TMs)
            {
                long aheadMs = effectiveMs - newest.TMs;
                if (aheadMs > maxExtrapolationMs)
                {
                    aheadMs = maxExtrapolationMs;
                }

                Vector3 extrapolated = ToVector(newest.Pos) + (ToVector(newest.Vel) * (aheadMs / 1000f));
                return new SampledPose(FromVector(extrapolated), newest.Rot);
            }

            // Interpolate within the bracketing snapshot pair.
            for (int i = 0; i < count - 1; i++)
            {
                track.TryGetSample(i, out EntitySample a);
                track.TryGetSample(i + 1, out EntitySample b);
                if (effectiveMs >= a.TMs && effectiveMs <= b.TMs)
                {
                    long spanMs = (long)b.TMs - a.TMs;
                    float fraction = spanMs <= 0 ? 0f : (float)(effectiveMs - a.TMs) / spanMs;

                    Vector3 pos = Vector3.Lerp(ToVector(a.Pos), ToVector(b.Pos), fraction);
                    Quaternion rot = Quaternion.Slerp(ToQuaternion(a.Rot), ToQuaternion(b.Rot), fraction);
                    return new SampledPose(FromVector(pos), FromQuaternion(rot));
                }
            }

            // Non-monotonic timestamps left no bracket; fall back to the newest known pose.
            return new SampledPose(newest.Pos, newest.Rot);
        }

        private static Vector3 ToVector(NetVec3 v)
        {
            return new Vector3(v.X, v.Y, v.Z);
        }

        private static NetVec3 FromVector(Vector3 v)
        {
            return new NetVec3(v.X, v.Y, v.Z);
        }

        private static Quaternion ToQuaternion(NetQuat q)
        {
            return new Quaternion(q.X, q.Y, q.Z, q.W);
        }

        private static NetQuat FromQuaternion(Quaternion q)
        {
            return new NetQuat(q.X, q.Y, q.Z, q.W);
        }
    }
}
