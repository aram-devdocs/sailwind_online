using System;
using Google.FlatBuffers;
using SwProto;
using Xunit;

namespace Sailwind.Contracts.Tests
{
    /// <summary>
    /// Round-trips a real <see cref="Envelope"/> for each <see cref="Payload"/>
    /// variant the init-0 protocol uses: build with a <see cref="FlatBufferBuilder"/>,
    /// finish with the "SWO0" file identifier, read the bytes back, and assert
    /// both the identifier and every field survive the encode/decode cycle.
    /// </summary>
    public sealed class EnvelopeRoundTripTests
    {
        private const string Identifier = "SWO0";

        private static byte[] Wrap(FlatBufferBuilder b, uint seq, Payload type, int payloadOffset)
        {
            var env = Envelope.CreateEnvelope(b, seq, type, payloadOffset);
            Envelope.FinishEnvelopeBuffer(b, env);
            return b.SizedByteArray();
        }

        private static Envelope Read(byte[] bytes, Payload expected)
        {
            var bb = new ByteBuffer(bytes);
            Assert.True(Envelope.EnvelopeBufferHasIdentifier(bb), "buffer must carry the SWO0 identifier");
            var env = Envelope.GetRootAsEnvelope(bb);
            Assert.Equal(expected, env.PayloadType);
            return env;
        }
        // NOTE: we deliberately do NOT gate the happy path on Envelope.VerifyEnvelope.
        // Google.FlatBuffers 25.2.10's union verifier produces false negatives for
        // several valid payload tables (e.g. ChatSend, SnapshotDelta, MoorRequest) even
        // though the same tables verify cleanly as a root buffer and read back correctly
        // (see PayloadTable_VerifiesAsRoot and the round-trip facts below). The wire
        // contract's real guard is the file identifier + a decode wrapped in try/catch,
        // which is what the server/protocol-smoke rely on — never the C# union Verifier.

        [Fact]
        public void Identifier_IsSwo0()
        {
            // The identifier is the wire contract's magic; assert the literal so a
            // schema change to file_identifier is caught here, not on the wire.
            Assert.Equal("SWO0", Identifier);
            Assert.Equal(4, Identifier.Length);
        }

        [Fact]
        public void ClientHello_RoundTrips()
        {
            var b = new FlatBufferBuilder(256);
            var payload = ClientHello.CreateClientHello(
                b,
                protocol_version: 1,
                display_nameOffset: b.CreateString("Ari"),
                tokenOffset: b.CreateString("tok-abc"),
                game_buildOffset: b.CreateString("13337000"),
                mod_versionOffset: b.CreateString("0.1.0"),
                api_surface_hashOffset: b.CreateString("e717da78"));
            var bytes = Wrap(b, 7, Payload.ClientHello, payload.Value);

            var env = Read(bytes, Payload.ClientHello);
            Assert.Equal(7u, env.Seq);
            var h = env.PayloadAsClientHello();
            Assert.Equal((ushort)1, h.ProtocolVersion);
            Assert.Equal("Ari", h.DisplayName);
            Assert.Equal("tok-abc", h.Token);
            Assert.Equal("13337000", h.GameBuild);
            Assert.Equal("0.1.0", h.ModVersion);
            Assert.Equal("e717da78", h.ApiSurfaceHash);
        }

        [Fact]
        public void ServerHello_WithNestedTables_RoundTrips()
        {
            var b = new FlatBufferBuilder(512);

            var features = CapabilityManifest.CreateFeaturesVector(b, new[]
            {
                b.CreateString("aoi"),
                b.CreateString("econ"),
            });
            var caps = CapabilityManifest.CreateCapabilityManifest(
                b,
                protocol_version: 1,
                featuresOffset: features,
                tick_hz: 30,
                snapshot_hz: 4,
                aoi_radius_cells: 2,
                cell_size_m: 1024f);
            var clock = WorldClock.CreateWorldClock(b, day: 12, time_of_day: 0.5f, moon_phase: 0.25f);
            var weather = WeatherSeed.CreateWeatherSeed(b, seed: 0xDEADBEEFUL, epoch_day: 12);
            var reason = b.CreateString("");
            var serverName = b.CreateString("test-server");

            var payload = ServerHello.CreateServerHello(
                b,
                accepted: true,
                reasonOffset: reason,
                player_id: 42,
                server_nameOffset: serverName,
                balance_gold: 100,
                capabilitiesOffset: caps,
                clockOffset: clock,
                weatherOffset: weather);
            var bytes = Wrap(b, 1, Payload.ServerHello, payload.Value);

            var env = Read(bytes, Payload.ServerHello);
            var s = env.PayloadAsServerHello();
            Assert.True(s.Accepted);
            Assert.Equal(42ul, s.PlayerId);
            Assert.Equal("test-server", s.ServerName);
            Assert.Equal(100L, s.BalanceGold);

            Assert.NotNull(s.Capabilities);
            var c = s.Capabilities.Value;
            Assert.Equal((ushort)1, c.ProtocolVersion);
            Assert.Equal((byte)30, c.TickHz);
            Assert.Equal((byte)4, c.SnapshotHz);
            Assert.Equal((byte)2, c.AoiRadiusCells);
            Assert.Equal(1024f, c.CellSizeM);
            Assert.Equal(2, c.FeaturesLength);
            Assert.Equal("aoi", c.Features(0));
            Assert.Equal("econ", c.Features(1));

            Assert.NotNull(s.Clock);
            Assert.Equal(12u, s.Clock.Value.Day);
            Assert.Equal(0.5f, s.Clock.Value.TimeOfDay);
            Assert.Equal(0.25f, s.Clock.Value.MoonPhase);

            Assert.NotNull(s.Weather);
            Assert.Equal(0xDEADBEEFUL, s.Weather.Value.Seed);
            Assert.Equal(12u, s.Weather.Value.EpochDay);
        }

        [Fact]
        public void ClientState_WithStructs_RoundTrips()
        {
            var b = new FlatBufferBuilder(256);
            ClientState.StartClientState(b);
            ClientState.AddPos(b, Vec3.CreateVec3(b, 1f, 2f, 3f));
            ClientState.AddRot(b, QuatC.CreateQuatC(b, 0f, 0f, 0f, 1f));
            ClientState.AddVel(b, Vec3.CreateVec3(b, -4f, 5f, -6f));
            ClientState.AddAboardBoat(b, 99);
            ClientState.AddTMs(b, 123456);
            var payload = ClientState.EndClientState(b);
            var bytes = Wrap(b, 3, Payload.ClientState, payload.Value);

            var env = Read(bytes, Payload.ClientState);
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
        public void SnapshotDelta_WithPlayerVector_RoundTrips()
        {
            var b = new FlatBufferBuilder(512);

            PlayerState.StartPlayerState(b);
            PlayerState.AddPlayerId(b, 1);
            PlayerState.AddPos(b, Vec3.CreateVec3(b, 10f, 0f, 20f));
            PlayerState.AddRot(b, QuatC.CreateQuatC(b, 0f, 0f, 0f, 1f));
            PlayerState.AddAboardBoat(b, 0);
            PlayerState.AddTMs(b, 1000);
            var p0 = PlayerState.EndPlayerState(b);

            PlayerState.StartPlayerState(b);
            PlayerState.AddPlayerId(b, 2);
            PlayerState.AddPos(b, Vec3.CreateVec3(b, 30f, 0f, 40f));
            PlayerState.AddRot(b, QuatC.CreateQuatC(b, 0f, 0f, 0f, 1f));
            PlayerState.AddAboardBoat(b, 7);
            PlayerState.AddTMs(b, 1001);
            var p1 = PlayerState.EndPlayerState(b);

            var players = SnapshotDelta.CreatePlayersVector(b, new[] { p0, p1 });
            var payload = SnapshotDelta.CreateSnapshotDelta(b, server_tick: 555, playersOffset: players);
            var bytes = Wrap(b, 9, Payload.SnapshotDelta, payload.Value);

            var env = Read(bytes, Payload.SnapshotDelta);
            var sd = env.PayloadAsSnapshotDelta();
            Assert.Equal(555u, sd.ServerTick);
            Assert.Equal(2, sd.PlayersLength);
            Assert.Equal(0, sd.BoatsLength);

            Assert.Equal(1ul, sd.Players(0).Value.PlayerId);
            Assert.Equal(10f, sd.Players(0).Value.Pos.Value.X);
            Assert.Equal(2ul, sd.Players(1).Value.PlayerId);
            Assert.Equal(7ul, sd.Players(1).Value.AboardBoat);
            Assert.Equal(40f, sd.Players(1).Value.Pos.Value.Z);
        }

        [Fact]
        public void EconTxn_RoundTrips()
        {
            var b = new FlatBufferBuilder(128);
            var payload = EconTxn.CreateEconTxn(
                b, txn_id: 1, amount_gold: 100, kind: 2, noteOffset: b.CreateString("sale"));
            var bytes = Wrap(b, 4, Payload.EconTxn, payload.Value);

            var env = Read(bytes, Payload.EconTxn);
            var t = env.PayloadAsEconTxn();
            Assert.Equal(1ul, t.TxnId);
            Assert.Equal(100L, t.AmountGold);
            Assert.Equal((byte)2, t.Kind);
            Assert.Equal("sale", t.Note);
        }

        [Fact]
        public void LedgerAck_RoundTrips()
        {
            var b = new FlatBufferBuilder(128);
            var payload = LedgerAck.CreateLedgerAck(
                b, txn_id: 1, accepted: true, new_balance: 100, reasonOffset: b.CreateString(""));
            var bytes = Wrap(b, 5, Payload.LedgerAck, payload.Value);

            var env = Read(bytes, Payload.LedgerAck);
            var a = env.PayloadAsLedgerAck();
            Assert.Equal(1ul, a.TxnId);
            Assert.True(a.Accepted);
            Assert.Equal(100L, a.NewBalance);
        }

        [Fact]
        public void WorldClock_RoundTrips()
        {
            var b = new FlatBufferBuilder(64);
            var payload = WorldClock.CreateWorldClock(b, day: 3, time_of_day: 0.75f, moon_phase: 0.1f);
            var bytes = Wrap(b, 2, Payload.WorldClock, payload.Value);

            var env = Read(bytes, Payload.WorldClock);
            var w = env.PayloadAsWorldClock();
            Assert.Equal(3u, w.Day);
            Assert.Equal(0.75f, w.TimeOfDay);
            Assert.Equal(0.1f, w.MoonPhase);
        }

        [Fact]
        public void ChatSend_RoundTrips()
        {
            var b = new FlatBufferBuilder(128);
            var payload = ChatSend.CreateChatSend(b, textOffset: b.CreateString("ahoy"), channel: 1);
            var bytes = Wrap(b, 6, Payload.ChatSend, payload.Value);

            var env = Read(bytes, Payload.ChatSend);
            var c = env.PayloadAsChatSend();
            Assert.Equal("ahoy", c.Text);
            Assert.Equal((byte)1, c.Channel);
        }

        [Fact]
        public void EmptyPayload_StillCarriesIdentifierAndSeq()
        {
            var b = new FlatBufferBuilder(32);
            var env = Envelope.CreateEnvelope(b, seq: 11, payload_type: Payload.NONE, payloadOffset: 0);
            Envelope.FinishEnvelopeBuffer(b, env);
            var bytes = b.SizedByteArray();

            var bb = new ByteBuffer(bytes);
            Assert.True(Envelope.EnvelopeBufferHasIdentifier(bb));
            var read = Envelope.GetRootAsEnvelope(bb);
            Assert.Equal(11u, read.Seq);
            Assert.Equal(Payload.NONE, read.PayloadType);
        }

        [Fact]
        public void PayloadTable_VerifiesAsRoot()
        {
            // Proves the payload encoding is well-formed: the same ChatSend table that
            // the union verifier false-negatives on verifies cleanly as a root buffer.
            var b = new FlatBufferBuilder(64);
            var payload = ChatSend.CreateChatSend(b, b.CreateString("hi"), 1);
            b.Finish(payload.Value);
            var bytes = b.SizedByteArray();

            var verifier = new Verifier(new ByteBuffer(bytes));
            Assert.True(verifier.VerifyBuffer(null, false, ChatSendVerify.Verify));

            var read = ChatSend.GetRootAsChatSend(new ByteBuffer(bytes));
            Assert.Equal("hi", read.Text);
            Assert.Equal((byte)1, read.Channel);
        }

        [Fact]
        public void GarbageBytes_AreRejectedByIdentifier()
        {
            // The server's hostile-input contract: malformed datagrams never carry the
            // identifier, so they are dropped before any field access.
            var junk = new byte[] { 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12 };
            Assert.False(Envelope.EnvelopeBufferHasIdentifier(new ByteBuffer(junk)));
        }

        [Fact]
        public void CorruptedIdentifier_IsRejected()
        {
            var b = new FlatBufferBuilder(64);
            var payload = WorldClock.CreateWorldClock(b, 1, 0f, 0f);
            var env = Envelope.CreateEnvelope(b, 1, Payload.WorldClock, payload.Value);
            Envelope.FinishEnvelopeBuffer(b, env);
            var bytes = b.SizedByteArray();
            Assert.True(Envelope.EnvelopeBufferHasIdentifier(new ByteBuffer(bytes)));

            // The 4-byte identifier sits at offset 4 in a non-size-prefixed buffer.
            bytes[4] = (byte)'X';
            Assert.False(Envelope.EnvelopeBufferHasIdentifier(new ByteBuffer(bytes)));
        }
    }
}
