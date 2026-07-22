using System;
using Sailwind.Online.Client.Sync;
using Xunit;

namespace Sailwind.Online.Sync.Tests
{
    /// <summary>
    /// The snapshot interpolation/extrapolation sampler: it turns the timestamped ring
    /// buffer into a single smoothed pose at a given render time. Pins position LERP +
    /// rotation SLERP between the bracketing snapshots, the render-behind interpolation
    /// delay, the velocity extrapolation cap, and the empty/single/before-oldest edges.
    /// renderTimeMs and the sample TMs share the wire-authored (ms) timeline.
    /// </summary>
    public sealed class InterpolatorTests
    {
        private const float Sin45 = 0.70710678f;
        private const float Cos45 = 0.70710678f;
        private const float Sin225 = 0.38268343f;
        private const float Cos225 = 0.92387953f;

        private static void AssertClose(float expected, float actual, float tol = 1e-3f)
        {
            Assert.True(Math.Abs(expected - actual) <= tol,
                $"expected {expected} but got {actual} (tol {tol})");
        }

        private static EntitySample Sample(uint tMs, NetVec3 pos, NetQuat rot = default, NetVec3 vel = default)
        {
            return new EntitySample { TMs = tMs, ReceivedMs = tMs, Pos = pos, Rot = rot, Vel = vel };
        }

        [Fact]
        public void SampleAt_NoSamples_ReturnsNone()
        {
            var track = new EntityTrack(id: 1, capacity: 8);

            Assert.Null(track.SampleAt(renderTimeMs: 1000));
        }

        [Fact]
        public void SampleAt_SingleSample_ReturnsThatSample()
        {
            var track = new EntityTrack(id: 1, capacity: 8);
            track.Push(Sample(1000, new NetVec3(5f, 6f, 7f), new NetQuat(0f, 0f, 0f, 1f)));

            SampledPose? pose = track.SampleAt(renderTimeMs: 9999);

            Assert.NotNull(pose);
            AssertClose(5f, pose!.Value.Pos.X);
            AssertClose(6f, pose.Value.Pos.Y);
            AssertClose(7f, pose.Value.Pos.Z);
        }

        [Fact]
        public void SampleAt_BeforeOldestSample_ClampsToOldest()
        {
            var track = new EntityTrack(id: 1, capacity: 8);
            track.Push(Sample(1000, new NetVec3(10f, 0f, 0f)));
            track.Push(Sample(1100, new NetVec3(20f, 0f, 0f)));

            // effective = renderTime - delay(100) = 900, before the oldest (1000): clamp.
            SampledPose? pose = track.SampleAt(renderTimeMs: 1000);

            Assert.NotNull(pose);
            AssertClose(10f, pose!.Value.Pos.X);
        }

        [Fact]
        public void SampleAt_ExactSample_ReturnsItWithoutBlending()
        {
            var track = new EntityTrack(id: 1, capacity: 8);
            track.Push(Sample(1000, new NetVec3(10f, 0f, 0f)));
            track.Push(Sample(1200, new NetVec3(30f, 0f, 0f)));

            // delay 0: effective lands exactly on each sample time.
            AssertClose(10f, track.SampleAt(renderTimeMs: 1000, interpolationDelayMs: 0)!.Value.Pos.X);
            AssertClose(30f, track.SampleAt(renderTimeMs: 1200, interpolationDelayMs: 0)!.Value.Pos.X);
        }

        [Fact]
        public void SampleAt_BetweenTwoSamples_LerpsPositionAndSlerpsRotation()
        {
            var track = new EntityTrack(id: 1, capacity: 8);
            track.Push(Sample(1000, new NetVec3(0f, 0f, 0f), new NetQuat(0f, 0f, 0f, 1f)));
            track.Push(Sample(1200, new NetVec3(20f, 0f, 0f), new NetQuat(0f, Sin45, 0f, Cos45)));

            // delay 0, effective = 1100 -> fraction 0.5 between the two snapshots.
            SampledPose? pose = track.SampleAt(renderTimeMs: 1100, interpolationDelayMs: 0);

            Assert.NotNull(pose);
            AssertClose(10f, pose!.Value.Pos.X);
            // Slerp of identity -> 90deg about Y at 0.5 is a 45deg rotation about Y.
            AssertClose(Sin225, pose.Value.Rot.Y);
            AssertClose(Cos225, pose.Value.Rot.W);
            AssertClose(0f, pose.Value.Rot.X);
            AssertClose(0f, pose.Value.Rot.Z);
        }

        [Fact]
        public void SampleAt_RenderBehindDelay_SamplesInThePast()
        {
            var track = new EntityTrack(id: 1, capacity: 8);
            track.Push(Sample(1000, new NetVec3(0f, 0f, 0f)));
            track.Push(Sample(1200, new NetVec3(20f, 0f, 0f)));

            // Default delay renders behind: effective 1100 -> interpolated midpoint, not the newest.
            AssertClose(10f, track.SampleAt(renderTimeMs: 1200, interpolationDelayMs: 100)!.Value.Pos.X);
            // No delay renders the newest sample directly.
            AssertClose(20f, track.SampleAt(renderTimeMs: 1200, interpolationDelayMs: 0)!.Value.Pos.X);
        }

        [Fact]
        public void SampleAt_PastNewestSample_ExtrapolatesFromVelocity()
        {
            var track = new EntityTrack(id: 1, capacity: 8);
            track.Push(Sample(1000, new NetVec3(0f, 0f, 0f)));
            // Newest moves at 10 units/sec along +X.
            track.Push(Sample(1100, new NetVec3(10f, 0f, 0f), vel: new NetVec3(10f, 0f, 0f)));

            // effective 1200 is 100ms past the newest: 10 + 10 * 0.1 = 11.
            SampledPose? pose = track.SampleAt(renderTimeMs: 1200, interpolationDelayMs: 0);

            Assert.NotNull(pose);
            AssertClose(11f, pose!.Value.Pos.X);
        }

        [Fact]
        public void SampleAt_ExtrapolationBeyondCap_IsClamped()
        {
            var track = new EntityTrack(id: 1, capacity: 8);
            track.Push(Sample(1000, new NetVec3(0f, 0f, 0f)));
            track.Push(Sample(1100, new NetVec3(10f, 0f, 0f), vel: new NetVec3(10f, 0f, 0f)));

            // effective 2000 is 900ms past the newest, but the cap (250ms) bounds it:
            // 10 + 10 * 0.25 = 12.5, not 10 + 10 * 0.9 = 19.
            SampledPose? pose = track.SampleAt(
                renderTimeMs: 2000, interpolationDelayMs: 0, maxExtrapolationMs: 250);

            Assert.NotNull(pose);
            AssertClose(12.5f, pose!.Value.Pos.X);
        }

        [Fact]
        public void Defaults_ExposeRenderBehindDelayAndExtrapolationCap()
        {
            Assert.Equal(100L, Interpolator.DefaultInterpolationDelayMs);
            Assert.Equal(250L, Interpolator.DefaultMaxExtrapolationMs);
        }
    }
}
