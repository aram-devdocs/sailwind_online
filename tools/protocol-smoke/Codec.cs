// Envelope encode/decode helpers for the protocol-smoke harness.
//
// Every datagram payload is one FlatBuffers Envelope (file_identifier "SWO0")
// carrying a sequence number and a Payload union. These helpers build the
// exact wire buffers the game client will send and decode the exact buffers
// the server sends back, using the committed Sailwind.Contracts bindings so
// the harness and the client cannot drift.

using System;
using Google.FlatBuffers;
using SwProto;

namespace Sailwind.ProtocolSmoke
{
    internal static class Codec
    {
        // Kept in step with contracts/PROTOCOL_VERSION. The schema deliberately
        // does not encode the version in the Envelope; it rides in the hello
        // exchange, so client and server refuse a mismatch at handshake.
        public const ushort ProtocolVersion = 1;

        public static byte[] EncodeClientHello(
            uint seq,
            string displayName,
            string token,
            string gameBuild,
            string modVersion,
            string apiSurfaceHash)
        {
            var b = new FlatBufferBuilder(256);
            var nameOff = b.CreateString(displayName ?? string.Empty);
            var tokenOff = b.CreateString(token ?? string.Empty);
            var gameBuildOff = b.CreateString(gameBuild ?? string.Empty);
            var modVersionOff = b.CreateString(modVersion ?? string.Empty);
            var apiHashOff = b.CreateString(apiSurfaceHash ?? string.Empty);
            var hello = ClientHello.CreateClientHello(
                b, ProtocolVersion, nameOff, tokenOff, gameBuildOff, modVersionOff, apiHashOff);
            var env = Envelope.CreateEnvelope(b, seq, Payload.ClientHello, hello.Value);
            Envelope.FinishEnvelopeBuffer(b, env);
            return b.SizedByteArray();
        }

        public static byte[] EncodeClientState(
            uint seq,
            float px, float py, float pz,
            float rx, float ry, float rz, float rw,
            float vx, float vy, float vz,
            ulong aboardBoat,
            uint tMs)
        {
            var b = new FlatBufferBuilder(128);
            ClientState.StartClientState(b);
            ClientState.AddPos(b, Vec3.CreateVec3(b, px, py, pz));
            ClientState.AddRot(b, QuatC.CreateQuatC(b, rx, ry, rz, rw));
            ClientState.AddVel(b, Vec3.CreateVec3(b, vx, vy, vz));
            ClientState.AddAboardBoat(b, aboardBoat);
            ClientState.AddTMs(b, tMs);
            var state = ClientState.EndClientState(b);
            var env = Envelope.CreateEnvelope(b, seq, Payload.ClientState, state.Value);
            Envelope.FinishEnvelopeBuffer(b, env);
            return b.SizedByteArray();
        }

        public static byte[] EncodeEconTxn(uint seq, ulong txnId, long amountGold, byte kind, string note)
        {
            var b = new FlatBufferBuilder(128);
            var noteOff = b.CreateString(note ?? string.Empty);
            var txn = EconTxn.CreateEconTxn(b, txnId, amountGold, kind, noteOff);
            var env = Envelope.CreateEnvelope(b, seq, Payload.EconTxn, txn.Value);
            Envelope.FinishEnvelopeBuffer(b, env);
            return b.SizedByteArray();
        }

        public static byte[] EncodeMoorRequest(
            uint seq,
            float px, float py, float pz,
            float rx, float ry, float rz, float rw,
            string name)
        {
            var b = new FlatBufferBuilder(128);
            var nameOff = b.CreateString(name ?? string.Empty);
            MoorRequest.StartMoorRequest(b);
            MoorRequest.AddPos(b, Vec3.CreateVec3(b, px, py, pz));
            MoorRequest.AddRot(b, QuatC.CreateQuatC(b, rx, ry, rz, rw));
            MoorRequest.AddName(b, nameOff);
            var req = MoorRequest.EndMoorRequest(b);
            var env = Envelope.CreateEnvelope(b, seq, Payload.MoorRequest, req.Value);
            Envelope.FinishEnvelopeBuffer(b, env);
            return b.SizedByteArray();
        }

        // Decodes a received datagram into an Envelope, or null when the bytes
        // are not a valid, identifier-tagged, verifier-clean Envelope.
        public static Envelope? TryDecodeEnvelope(byte[] data)
        {
            if (data == null || data.Length < 8)
            {
                return null;
            }

            var bb = new ByteBuffer(data);
            if (!Envelope.EnvelopeBufferHasIdentifier(bb))
            {
                return null;
            }

            if (!Envelope.VerifyEnvelope(bb))
            {
                return null;
            }

            return Envelope.GetRootAsEnvelope(bb);
        }
    }
}
