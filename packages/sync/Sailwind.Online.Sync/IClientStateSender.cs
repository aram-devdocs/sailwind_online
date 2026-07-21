using Sailwind.Api;

namespace Sailwind.Online.Client.Sync
{
    /// <summary>
    /// The slice of the network client that <see cref="StateReporter"/> drives. Declaring
    /// it here (rather than referencing the transport directly) keeps the dependency edge
    /// one-way — the transport package references sync, never the reverse — and lets the
    /// reporter's cadence be unit-tested against a fake sender.
    /// </summary>
    public interface IClientStateSender
    {
        /// <summary>True once the server handshake has completed.</summary>
        bool HandshakeComplete { get; }

        /// <summary>Monotonic millisecond clock shared by the transport.</summary>
        long NowMs { get; }

        /// <summary>Snapshot rate advertised by the server (Hz).</summary>
        byte SnapshotHz { get; }

        /// <summary>Report the local boat's absolute-world pose to the server as a ClientState.</summary>
        void SendClientState(BoatPose pose);
    }
}
