using Google.FlatBuffers;
using Sailwind.Online.Client.Net;
using SwProto;
using Xunit;

namespace Sailwind.Online.Net.Tests
{
    /// <summary>
    /// Exercises the pure envelope codec: the client-authored ClientHello / ClientState
    /// encoders round-trip through the real FlatBuffers accessors, and the receive-side
    /// dispatcher routes every server payload (and rejects hostile input) exactly as the
    /// live client relies on.
    /// </summary>
    public sealed class CodecTests
    {
        private sealed class CapturingHandler : IServerMessageHandler
        {
            public int Calls;
            public uint LastSeq;
            public ServerHello? Hello;
            public WorldClock? Clock;
            public SnapshotDelta? Delta;
            public Payload UnhandledType;

            public void OnServerHello(ServerHello hello, uint seq) { Hello = hello; LastSeq = seq; Calls++; }
            public void OnSnapshotDelta(SnapshotDelta delta, uint seq) { Delta = delta; LastSeq = seq; Calls++; }
            public void OnAoiUpdate(AoiUpdate update, uint seq) { LastSeq = seq; Calls++; }
            public void OnCellSnapshot(CellSnapshot snapshot, uint seq) { LastSeq = seq; Calls++; }
            public void OnWorldClock(WorldClock clock, uint seq) { Clock = clock; LastSeq = seq; Calls++; }
            public void OnUnhandled(Payload payloadType, uint seq) { UnhandledType = payloadType; LastSeq = seq; Calls++; }
        }

        [Fact]
        public void EncodeClientHello_RoundTripsThroughEnvelope()
        {
            var codec = new Codec();
            byte[] bytes = codec.EncodeClientHello(
                seq: 7,
                protocolVersion: 1,
                displayName: "Ari",
                token: "tok-abc",
                gameBuild: "13337000",
                modVersion: "0.1.0",
                apiSurfaceHash: "e717da78");

            var bb = new ByteBuffer(bytes);
            Assert.True(Envelope.EnvelopeBufferHasIdentifier(bb));
            var env = Envelope.GetRootAsEnvelope(bb);
            Assert.Equal(7u, env.Seq);
            Assert.Equal(Payload.ClientHello, env.PayloadType);

            var h = env.PayloadAsClientHello();
            Assert.Equal((ushort)1, h.ProtocolVersion);
            Assert.Equal("Ari", h.DisplayName);
            Assert.Equal("tok-abc", h.Token);
            Assert.Equal("13337000", h.GameBuild);
            Assert.Equal("0.1.0", h.ModVersion);
            Assert.Equal("e717da78", h.ApiSurfaceHash);
        }

        [Fact]
        public void EncodeClientHello_NullStrings_EncodeAsEmpty()
        {
            var codec = new Codec();
            byte[] bytes = codec.EncodeClientHello(1, 1, null, null, null, null, null);

            var env = Envelope.GetRootAsEnvelope(new ByteBuffer(bytes));
            var h = env.PayloadAsClientHello();
            Assert.Equal(string.Empty, h.DisplayName);
            Assert.Equal(string.Empty, h.Token);
            Assert.Equal(string.Empty, h.GameBuild);
            Assert.Equal(string.Empty, h.ModVersion);
            Assert.Equal(string.Empty, h.ApiSurfaceHash);
        }

        [Fact]
        public void EncodeClientState_RoundTripsTransform()
        {
            var codec = new Codec();
            byte[] bytes = codec.EncodeClientState(
                seq: 3,
                px: 1f, py: 2f, pz: 3f,
                rx: 0f, ry: 0f, rz: 0f, rw: 1f,
                vx: -4f, vy: 5f, vz: -6f,
                aboardBoat: 99,
                tMs: 123456);

            var env = Envelope.GetRootAsEnvelope(new ByteBuffer(bytes));
            Assert.Equal(3u, env.Seq);
            Assert.Equal(Payload.ClientState, env.PayloadType);

            var cs = env.PayloadAsClientState();
            Assert.NotNull(cs.Pos);
            Assert.Equal(1f, cs.Pos.Value.X);
            Assert.Equal(2f, cs.Pos.Value.Y);
            Assert.Equal(3f, cs.Pos.Value.Z);
            Assert.NotNull(cs.Rot);
            Assert.Equal(1f, cs.Rot.Value.W);
            Assert.NotNull(cs.Vel);
            Assert.Equal(-4f, cs.Vel.Value.X);
            Assert.Equal(-6f, cs.Vel.Value.Z);
            Assert.Equal(99ul, cs.AboardBoat);
            Assert.Equal(123456u, cs.TMs);
        }

        [Fact]
        public void TryDispatch_ServerHello_RoutesToHandler()
        {
            var b = new FlatBufferBuilder(256);
            var reason = b.CreateString(string.Empty);
            var serverName = b.CreateString("test-server");
            var hello = ServerHello.CreateServerHello(
                b, accepted: true, reasonOffset: reason, player_id: 42,
                server_nameOffset: serverName, balance_gold: 0);
            byte[] bytes = Wrap(b, 5, Payload.ServerHello, hello.Value);

            var handler = new CapturingHandler();
            var codec = new Codec();
            Assert.True(codec.TryDispatch(bytes, handler));

            Assert.Equal(1, handler.Calls);
            Assert.Equal(5u, handler.LastSeq);
            Assert.NotNull(handler.Hello);
            Assert.True(handler.Hello.Value.Accepted);
            Assert.Equal(42ul, handler.Hello.Value.PlayerId);
        }

        [Fact]
        public void TryDispatch_WorldClock_RoutesToHandler()
        {
            var b = new FlatBufferBuilder(64);
            var clock = WorldClock.CreateWorldClock(b, day: 12, time_of_day: 0.5f, moon_phase: 0.25f);
            byte[] bytes = Wrap(b, 9, Payload.WorldClock, clock.Value);

            var handler = new CapturingHandler();
            Assert.True(new Codec().TryDispatch(bytes, handler));

            Assert.NotNull(handler.Clock);
            Assert.Equal(12u, handler.Clock.Value.Day);
            Assert.Equal(0.5f, handler.Clock.Value.TimeOfDay);
        }

        [Fact]
        public void TryDispatch_DeferredPayload_RoutesToUnhandled()
        {
            var b = new FlatBufferBuilder(128);
            var ack = LedgerAck.CreateLedgerAck(
                b, txn_id: 1, accepted: true, new_balance: 100, reasonOffset: b.CreateString(string.Empty));
            byte[] bytes = Wrap(b, 4, Payload.LedgerAck, ack.Value);

            var handler = new CapturingHandler();
            Assert.True(new Codec().TryDispatch(bytes, handler));

            Assert.Equal(1, handler.Calls);
            Assert.Equal(Payload.LedgerAck, handler.UnhandledType);
            Assert.Equal(4u, handler.LastSeq);
        }

        [Fact]
        public void TryDispatch_GarbageBytes_ReturnsFalse()
        {
            var junk = new byte[] { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12 };
            var handler = new CapturingHandler();
            Assert.False(new Codec().TryDispatch(junk, handler));
            Assert.Equal(0, handler.Calls);
        }

        [Fact]
        public void TryDispatch_NullOrTooShort_ReturnsFalse()
        {
            var handler = new CapturingHandler();
            var codec = new Codec();
            Assert.False(codec.TryDispatch(null, handler));
            Assert.False(codec.TryDispatch(new byte[] { 1, 2 }, handler));
            Assert.Equal(0, handler.Calls);
        }

        private static byte[] Wrap(FlatBufferBuilder b, uint seq, Payload type, int payloadOffset)
        {
            var env = Envelope.CreateEnvelope(b, seq, type, payloadOffset);
            Envelope.FinishEnvelopeBuffer(b, env);
            return b.SizedByteArray();
        }
    }
}
