using Google.FlatBuffers;
using SwProto;

namespace Sailwind.Online.Client.Net
{
    /// <summary>
    /// Receives the server-to-client payloads the init-0 client consumes. Payloads that belong to
    /// deferred features (moorage acks, ledger acks, chat broadcasts) are routed to
    /// <see cref="OnUnhandled"/> rather than silently dropped.
    /// </summary>
    public interface IServerMessageHandler
    {
        void OnServerHello(ServerHello hello, uint seq);
        void OnSnapshotDelta(SnapshotDelta delta, uint seq);
        void OnAoiUpdate(AoiUpdate update, uint seq);
        void OnCellSnapshot(CellSnapshot snapshot, uint seq);
        void OnWorldClock(WorldClock clock, uint seq);
        void OnUnhandled(Payload payloadType, uint seq);
    }

    /// <summary>
    /// Envelope encode/decode over a single pooled <see cref="FlatBufferBuilder"/>.
    /// All calls happen on the Unity main thread (LiteNetLib is polled there), so one reused
    /// builder is safe and keeps per-send allocation to the returned byte[] only.
    /// </summary>
    public sealed class Codec
    {
        private readonly FlatBufferBuilder _builder = new FlatBufferBuilder(256);

        /// <summary>Build a ClientHello envelope. Null strings are encoded as empty.</summary>
        public byte[] EncodeClientHello(
            uint seq,
            ushort protocolVersion,
            string? displayName,
            string? token,
            string? gameBuild,
            string? modVersion,
            string? apiSurfaceHash)
        {
            FlatBufferBuilder b = _builder;
            b.Clear();

            StringOffset dn = b.CreateString(displayName ?? string.Empty);
            StringOffset tk = b.CreateString(token ?? string.Empty);
            StringOffset gb = b.CreateString(gameBuild ?? string.Empty);
            StringOffset mv = b.CreateString(modVersion ?? string.Empty);
            StringOffset ah = b.CreateString(apiSurfaceHash ?? string.Empty);

            Offset<ClientHello> hello = ClientHello.CreateClientHello(b, protocolVersion, dn, tk, gb, mv, ah);
            Offset<Envelope> env = Envelope.CreateEnvelope(b, seq, Payload.ClientHello, hello.Value);
            Envelope.FinishEnvelopeBuffer(b, env);
            return b.SizedByteArray();
        }

        /// <summary>Build a ClientState envelope from raw transform components.</summary>
        public byte[] EncodeClientState(
            uint seq,
            float px, float py, float pz,
            float rx, float ry, float rz, float rw,
            float vx, float vy, float vz,
            ulong aboardBoat,
            uint tMs)
        {
            FlatBufferBuilder b = _builder;
            b.Clear();

            ClientState.StartClientState(b);
            ClientState.AddPos(b, Vec3.CreateVec3(b, px, py, pz));
            ClientState.AddRot(b, QuatC.CreateQuatC(b, rx, ry, rz, rw));
            ClientState.AddVel(b, Vec3.CreateVec3(b, vx, vy, vz));
            ClientState.AddAboardBoat(b, aboardBoat);
            ClientState.AddTMs(b, tMs);
            Offset<ClientState> state = ClientState.EndClientState(b);

            Offset<Envelope> env = Envelope.CreateEnvelope(b, seq, Payload.ClientState, state.Value);
            Envelope.FinishEnvelopeBuffer(b, env);
            return b.SizedByteArray();
        }

        /// <summary>
        /// Verify and dispatch one received datagram. Returns false for anything that is not a
        /// well-formed Envelope (the hostile-input guard); malformed packets never reach a handler.
        /// </summary>
        public bool TryDispatch(byte[]? data, IServerMessageHandler handler)
        {
            if (data == null || data.Length < 4)
            {
                return false;
            }

            ByteBuffer bb = new ByteBuffer(data);
            if (!Envelope.EnvelopeBufferHasIdentifier(bb))
            {
                return false;
            }

            if (!Envelope.VerifyEnvelope(bb))
            {
                return false;
            }

            Envelope env = Envelope.GetRootAsEnvelope(bb);
            uint seq = env.Seq;

            switch (env.PayloadType)
            {
                case Payload.ServerHello:
                    handler.OnServerHello(env.PayloadAsServerHello(), seq);
                    return true;
                case Payload.SnapshotDelta:
                    handler.OnSnapshotDelta(env.PayloadAsSnapshotDelta(), seq);
                    return true;
                case Payload.AoiUpdate:
                    handler.OnAoiUpdate(env.PayloadAsAoiUpdate(), seq);
                    return true;
                case Payload.CellSnapshot:
                    handler.OnCellSnapshot(env.PayloadAsCellSnapshot(), seq);
                    return true;
                case Payload.WorldClock:
                    handler.OnWorldClock(env.PayloadAsWorldClock(), seq);
                    return true;
                default:
                    handler.OnUnhandled(env.PayloadType, seq);
                    return true;
            }
        }
    }
}
