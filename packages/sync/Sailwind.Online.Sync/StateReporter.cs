using Sailwind.Api;

namespace Sailwind.Online.Client.Sync
{
    /// <summary>
    /// Paces client-to-server state reporting at the server-advertised snapshot rate.
    /// When the local player is aboard a boat, it reads the absolute-world <see cref="BoatPose"/>
    /// from Sailwind.API and sends it as a ClientState. Driven from the Unity main thread.
    /// </summary>
    public sealed class StateReporter
    {
        private const int DefaultSnapshotHz = 4;

        private readonly IClientStateSender _net;
        private long _nextSendMs;

        public StateReporter(IClientStateSender net)
        {
            _net = net;
        }

        /// <summary>Send at most one ClientState this frame if the cadence and world state allow it.</summary>
        public void Tick()
        {
            if (!_net.HandshakeComplete)
            {
                return;
            }

            long now = _net.NowMs;
            if (now < _nextSendMs)
            {
                return;
            }

            byte hz = _net.SnapshotHz;
            int intervalMs = hz > 0 ? 1000 / hz : 1000 / DefaultSnapshotHz;
            _nextSendMs = now + intervalMs;

            IPlayerBoatReader? boat = SailwindApi.PlayerBoat;
            if (boat == null || !boat.HasBoat)
            {
                return;
            }

            _net.SendClientState(boat.Pose);
        }
    }
}
