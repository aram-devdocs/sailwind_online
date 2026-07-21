using System;
using System.Numerics;
using Sailwind.Api;
using Sailwind.Online.Client.Sync;
using Xunit;

namespace Sailwind.Online.Sync.Tests
{
    /// <summary>
    /// StateReporter cadence: it stays silent before the handshake, paces sends at the
    /// server-advertised snapshot rate (falling back to the default when the rate is 0),
    /// skips sending when the player has no boat, and forwards the boat pose verbatim.
    /// The reporter reads the local boat from the SailwindApi facade, which each test arms.
    /// </summary>
    public sealed class StateReporterTests : IDisposable
    {
        public void Dispose() => SailwindApi.PlayerBoat = null;

        private sealed class FakeSender : IClientStateSender
        {
            public bool HandshakeComplete { get; set; }
            public long NowMs { get; set; }
            public byte SnapshotHz { get; set; }
            public int SendCount { get; private set; }
            public BoatPose LastPose { get; private set; }

            public void SendClientState(BoatPose pose)
            {
                LastPose = pose;
                SendCount++;
            }
        }

        private sealed class FakeBoat : IPlayerBoatReader
        {
            public bool HasBoat { get; set; }
            public BoatPose Pose { get; set; }
        }

        [Fact]
        public void Tick_BeforeHandshake_DoesNotSend()
        {
            var sender = new FakeSender { HandshakeComplete = false, NowMs = 1000, SnapshotHz = 4 };
            SailwindApi.PlayerBoat = new FakeBoat { HasBoat = true };
            var reporter = new StateReporter(sender);

            reporter.Tick();

            Assert.Equal(0, sender.SendCount);
        }

        [Fact]
        public void Tick_SendsOnceThenThrottlesUntilNextInterval()
        {
            var sender = new FakeSender { HandshakeComplete = true, NowMs = 0, SnapshotHz = 4 }; // 250 ms
            SailwindApi.PlayerBoat = new FakeBoat { HasBoat = true };
            var reporter = new StateReporter(sender);

            reporter.Tick();
            Assert.Equal(1, sender.SendCount);

            sender.NowMs = 100; // still inside the 250 ms window
            reporter.Tick();
            Assert.Equal(1, sender.SendCount);

            sender.NowMs = 250; // window elapsed
            reporter.Tick();
            Assert.Equal(2, sender.SendCount);
        }

        [Fact]
        public void Tick_ZeroSnapshotHz_FallsBackToDefaultCadence()
        {
            var sender = new FakeSender { HandshakeComplete = true, NowMs = 0, SnapshotHz = 0 };
            SailwindApi.PlayerBoat = new FakeBoat { HasBoat = true };
            var reporter = new StateReporter(sender);

            reporter.Tick(); // default 4 Hz => 250 ms interval
            Assert.Equal(1, sender.SendCount);

            sender.NowMs = 249;
            reporter.Tick();
            Assert.Equal(1, sender.SendCount);

            sender.NowMs = 250;
            reporter.Tick();
            Assert.Equal(2, sender.SendCount);
        }

        [Fact]
        public void Tick_NoBoat_DoesNotSend()
        {
            var sender = new FakeSender { HandshakeComplete = true, NowMs = 0, SnapshotHz = 4 };
            SailwindApi.PlayerBoat = new FakeBoat { HasBoat = false };
            var reporter = new StateReporter(sender);

            reporter.Tick();

            Assert.Equal(0, sender.SendCount);
        }

        [Fact]
        public void Tick_NoApiReader_DoesNotSend()
        {
            var sender = new FakeSender { HandshakeComplete = true, NowMs = 0, SnapshotHz = 4 };
            SailwindApi.PlayerBoat = null;
            var reporter = new StateReporter(sender);

            reporter.Tick();

            Assert.Equal(0, sender.SendCount);
        }

        [Fact]
        public void Tick_ForwardsBoatPoseVerbatim()
        {
            var pose = new BoatPose(
                new Vector3(1f, 2f, 3f),
                Quaternion.Identity,
                new Vector3(4f, 5f, 6f));
            var sender = new FakeSender { HandshakeComplete = true, NowMs = 0, SnapshotHz = 4 };
            SailwindApi.PlayerBoat = new FakeBoat { HasBoat = true, Pose = pose };
            var reporter = new StateReporter(sender);

            reporter.Tick();

            Assert.Equal(1, sender.SendCount);
            Assert.Equal(pose.Position, sender.LastPose.Position);
            Assert.Equal(pose.Velocity, sender.LastPose.Velocity);
        }
    }
}
