using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Sockets;
using System.Numerics;
using Google.FlatBuffers;
using Sailwind.Api;
using Sailwind.Online.Client.Net;
using SwProto;
using Xunit;

namespace Sailwind.Online.Net.Tests
{
    /// <summary>
    /// Exercises the <see cref="NetClient"/> session state machine against a <see cref="MockTransport"/>
    /// and a controllable clock: transport-up to Ready, the 250 ms ClientHello resend loop, the
    /// exponential reconnect backoff on disconnect, the keepalive/Ping surface, and receive dispatch
    /// through the real <see cref="Codec"/>. No sockets, no wall-clock waits.
    /// </summary>
    public sealed class NetClientTests
    {
        private static readonly ConnectOptions Options = new ConnectOptions
        {
            Host = "test-host",
            Port = 4242,
            DisplayName = "Ari",
            Token = "tok",
            GameBuild = "build",
            ModVersion = "0.1.0",
            ApiSurfaceHash = "hash"
        };

        [Fact]
        public void Connect_StartsTransportAndOpensPeer_EntersConnecting()
        {
            var transport = new MockTransport();
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);

            net.Connect(Options);

            Assert.Equal(1, transport.StartCalls);
            Assert.Equal(1, transport.ConnectCalls);
            Assert.Equal("test-host", transport.LastHost);
            Assert.Equal(4242, transport.LastPort);
            Assert.Equal(NetClient.ConnectKey, transport.LastKey);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);
        }

        [Fact]
        public void Connect_SameOptionsWhilePending_DoesNotOpenDuplicatePeer()
        {
            var transport = new MockTransport();
            var net = new NetClient(new NullNetLog(), transport, () => 0);

            net.Connect(Options);
            net.Connect(Options);

            Assert.Equal(1, transport.StartCalls);
            Assert.Equal(1, transport.ConnectCalls);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);

            transport.RaisePeerConnected();
            transport.RaiseNetworkReceive(ServerHelloEnvelope(
                accepted: true,
                playerId: 77,
                snapshotHz: 8));

            Assert.Equal(ConnectionStatus.Ready, net.Status);
        }

        [Fact]
        public void Connect_WhenTransportStartFails_LogsAndDoesNotOpenPeer()
        {
            var transport = new MockTransport { StartResult = false };
            var log = new RecordingLog();
            var net = new NetClient(log, transport, () => 0);

            net.Connect(Options);

            Assert.Equal(1, transport.StartCalls);
            Assert.Equal(0, transport.ConnectCalls);
            Assert.Equal(ConnectionStatus.Disconnected, net.Status);
            Assert.NotEmpty(log.Errors);
        }

        [Fact]
        public void Connect_WhenTransportStartFails_RetriesStartBeforeOpeningPeer()
        {
            var transport = new MockTransport();
            transport.StartResults.Enqueue(false);
            transport.StartResults.Enqueue(true);
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);

            net.Connect(Options);

            now = NetClient.DefaultReconnectMs - 1;
            net.Poll();
            Assert.Equal(1, transport.StartCalls);
            Assert.Equal(0, transport.ConnectCalls);
            Assert.False(transport.IsRunning);
            Assert.Equal(ConnectionStatus.Disconnected, net.Status);

            now = NetClient.DefaultReconnectMs;
            net.Poll();
            Assert.Equal(2, transport.StartCalls);
            Assert.Equal(1, transport.ConnectCalls);
            Assert.True(transport.IsRunning);
            Assert.Equal(Options.Host, transport.LastHost);
            Assert.Equal(Options.Port, transport.LastPort);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);
        }

        [Fact]
        public void Connect_WhileHandshaking_RestartsSameOptionsWithFreshPeer()
        {
            var transport = new MockTransport();
            var net = new NetClient(new NullNetLog(), transport, () => 0);
            net.Connect(Options);
            transport.RaisePeerConnected();
            Assert.Equal(ConnectionStatus.Handshaking, net.Status);
            Assert.Single(transport.Sent);

            net.Connect(Options);

            Assert.Equal(1, transport.DropPeerCalls);
            Assert.Equal(2, transport.ConnectCalls);
            Assert.Equal(2, transport.FreshPeerConnectCalls);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);
            Assert.Single(transport.Sent);

            transport.RaisePeerConnected();

            Assert.Equal(ConnectionStatus.Handshaking, net.Status);
            Assert.Equal(2, transport.Sent.Count);
            Assert.Equal(Options.DisplayName, Decode(transport.Sent[1]).PayloadAsClientHello().DisplayName);
        }

        [Fact]
        public void Connect_WhileReady_RestartsWithNewOptionsAndClearsSession()
        {
            var transport = new MockTransport();
            var log = new RecordingLog();
            var net = new NetClient(log, transport, () => 100);
            net.Connect(Options);
            transport.RaisePeerConnected();
            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 77, snapshotHz: 8));
            net.SendClientState(new BoatPose());
            byte[] snapshot = SnapshotDeltaEnvelope(1, 88, 1f, 2f, 3f, 0f, 0f, 0f, 1f, 123, 10);
            transport.RaiseNetworkReceive(snapshot);
            Assert.True(net.Cache.TryGetPlayer(88, out _));

            var replacement = new ConnectOptions
            {
                Host = "replacement-host",
                Port = 5252,
                DisplayName = "Bea",
                Token = "replacement-token",
                GameBuild = "replacement-build",
                ModVersion = "0.2.0",
                ApiSurfaceHash = "replacement-hash"
            };
            transport.Sent.Clear();

            net.Connect(replacement);

            Assert.Equal(1, transport.DropPeerCalls);
            Assert.Equal(2, transport.ConnectCalls);
            Assert.Equal(2, transport.FreshPeerConnectCalls);
            Assert.Equal("replacement-host", transport.LastHost);
            Assert.Equal(5252, transport.LastPort);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);
            Assert.Equal(0ul, net.PlayerId);
            Assert.Equal((byte)4, net.SnapshotHz);
            Assert.False(net.Cache.TryGetPlayer(88, out _));
            Assert.Empty(transport.Sent);

            transport.RaisePeerConnected();

            Assert.Equal(ConnectionStatus.Handshaking, net.Status);
            ClientHello hello = Decode(Assert.Single(transport.Sent)).PayloadAsClientHello();
            Assert.Equal("Bea", hello.DisplayName);
            Assert.Equal("replacement-token", hello.Token);

            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 99, snapshotHz: 12));
            net.SendClientState(new BoatPose());
            transport.RaiseNetworkReceive(snapshot);

            Assert.Equal(ConnectionStatus.Ready, net.Status);
            Assert.Equal(99ul, net.PlayerId);
            Assert.Equal((byte)12, net.SnapshotHz);
            Assert.Equal(2, log.Infos.FindAll(message => message.Contains("First outbound position")).Count);
            Assert.Equal(2, log.Infos.FindAll(message => message.Contains("First inbound position")).Count);
            Assert.DoesNotContain(log.Infos, message => message.Contains(replacement.Token));
        }

        [Fact]
        public void Connect_Null_ThrowsWithoutChangingCurrentAttempt()
        {
            var transport = new MockTransport();
            var net = new NetClient(new NullNetLog(), transport, () => 0);
            net.Connect(Options);

            Assert.Throws<ArgumentNullException>(() => net.Connect(null));

            Assert.Equal(1, transport.StartCalls);
            Assert.Equal(1, transport.ConnectCalls);
            Assert.Equal(0, transport.DropPeerCalls);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);
            Assert.Equal(Options.Host, transport.LastHost);
            Assert.Equal(Options.Port, transport.LastPort);

            transport.RaisePeerConnected();

            Assert.Equal(ConnectionStatus.Handshaking, net.Status);
            ClientHello hello = Decode(Assert.Single(transport.Sent)).PayloadAsClientHello();
            Assert.Equal(Options.DisplayName, hello.DisplayName);
            Assert.Equal(Options.Token, hello.Token);
        }

        [Fact]
        public void Connect_WhenTransportCreatesNoPeer_RemainsDisconnectedUntilRetrySucceeds()
        {
            var transport = new MockTransport { ConnectResult = false };
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);

            net.Connect(Options);

            Assert.True(transport.IsRunning);
            Assert.Equal(1, transport.StartCalls);
            Assert.Equal(1, transport.ConnectCalls);
            Assert.Equal(0, transport.FreshPeerConnectCalls);
            Assert.Equal(ConnectionStatus.Disconnected, net.Status);

            transport.ConnectResult = true;
            now = NetClient.DefaultReconnectMs - 1;
            net.Poll();
            Assert.Equal(1, transport.ConnectCalls);
            Assert.Equal(ConnectionStatus.Disconnected, net.Status);

            now = NetClient.DefaultReconnectMs;
            net.Poll();
            Assert.Equal(2, transport.ConnectCalls);
            Assert.Equal(1, transport.FreshPeerConnectCalls);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);
        }

        [Fact]
        public void Handshake_PeerConnectedThenServerHello_ReachesReady()
        {
            var transport = new MockTransport();
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);
            net.Connect(Options);

            transport.RaisePeerConnected();

            Assert.Equal(ConnectionStatus.Handshaking, net.Status);
            Assert.Single(transport.Sent);
            Envelope hello = Decode(transport.Sent[0]);
            Assert.Equal(Payload.ClientHello, hello.PayloadType);
            Assert.Equal("Ari", hello.PayloadAsClientHello().DisplayName);

            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 77, snapshotHz: 8));

            Assert.Equal(ConnectionStatus.Ready, net.Status);
            Assert.True(net.HandshakeComplete);
            Assert.Equal(77ul, net.PlayerId);
            Assert.Equal((byte)8, net.SnapshotHz);
        }

        [Fact]
        public void Handshake_ServerHelloRejected_ReturnsToDisconnected()
        {
            var transport = new MockTransport();
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);
            net.Connect(Options);
            transport.RaisePeerConnected();

            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: false, playerId: 0, snapshotHz: 0));

            Assert.Equal(ConnectionStatus.Disconnected, net.Status);
            Assert.False(net.HandshakeComplete);
            Assert.Equal(1, transport.DropPeerCalls);
            Assert.False(transport.IsPeerConnected);

            now = NetClient.DefaultReconnectMs - 1;
            net.Poll();
            Assert.Equal(1, transport.ConnectCalls);

            now = NetClient.DefaultReconnectMs;
            net.Poll();
            Assert.Equal(2, transport.ConnectCalls);
            Assert.Equal(2, transport.FreshPeerConnectCalls);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);

            transport.RaisePeerConnected();
            Assert.Equal(ConnectionStatus.Handshaking, net.Status);
            Assert.Equal(2, transport.Sent.Count);
        }

        [Fact]
        public void Handshake_ConsecutiveRejections_BackOffUntilAcceptedHandshakeResets()
        {
            var transport = new MockTransport();
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);
            net.Connect(Options);

            long[] rejectionGaps =
            {
                NetClient.DefaultReconnectMs,
                2 * NetClient.DefaultReconnectMs,
                4 * NetClient.DefaultReconnectMs,
                NetClient.MaxReconnectMs,
                NetClient.MaxReconnectMs
            };
            int expectedConnectCalls = 1;

            for (int i = 0; i < rejectionGaps.Length; i++)
            {
                transport.RaisePeerConnected();
                transport.RaiseNetworkReceive(ServerHelloEnvelope(
                    accepted: false,
                    playerId: 0,
                    snapshotHz: 0));

                Assert.Equal(ConnectionStatus.Disconnected, net.Status);
                Assert.Equal(i + 1, transport.DropPeerCalls);

                now += rejectionGaps[i] - 1;
                net.Poll();
                Assert.Equal(expectedConnectCalls, transport.ConnectCalls);

                now++;
                net.Poll();
                expectedConnectCalls++;
                Assert.Equal(expectedConnectCalls, transport.ConnectCalls);
                Assert.Equal(ConnectionStatus.Connecting, net.Status);
            }

            transport.RaisePeerConnected();
            transport.RaiseNetworkReceive(ServerHelloEnvelope(
                accepted: true,
                playerId: 77,
                snapshotHz: 8));

            Assert.Equal(ConnectionStatus.Ready, net.Status);
            Assert.Equal(rejectionGaps.Length, transport.DropPeerCalls);

            transport.RaisePeerDisconnected();
            now += NetClient.DefaultReconnectMs - 1;
            net.Poll();
            Assert.Equal(expectedConnectCalls, transport.ConnectCalls);

            now++;
            net.Poll();
            expectedConnectCalls++;
            Assert.Equal(expectedConnectCalls, transport.ConnectCalls);
            Assert.Equal(rejectionGaps.Length, transport.DropPeerCalls);
        }

        [Fact]
        public void Handshake_RejectedThenQueuedAcceptedHello_IgnoresStaleAcceptance()
        {
            var transport = new MockTransport();
            var log = new RecordingLog();
            long now = 0;
            var net = new NetClient(log, transport, () => now);
            net.Connect(Options);
            transport.RaisePeerConnected();
            transport.DropPeerCallback = () => transport.RaiseNetworkReceive(
                ServerHelloEnvelope(accepted: true, playerId: 99, snapshotHz: 12));

            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: false, playerId: 0, snapshotHz: 0));

            Assert.Equal(ConnectionStatus.Disconnected, net.Status);
            Assert.False(net.HandshakeComplete);
            Assert.Equal(0ul, net.PlayerId);
            Assert.Equal((byte)4, net.SnapshotHz);
            Assert.Equal(1, transport.DropPeerCalls);
            Assert.Single(log.Warnings);
            Assert.Single(log.Debugs);

            now = NetClient.DefaultReconnectMs - 1;
            net.Poll();
            Assert.Equal(1, transport.ConnectCalls);

            now = NetClient.DefaultReconnectMs;
            net.Poll();
            Assert.Equal(2, transport.ConnectCalls);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);

            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 101, snapshotHz: 16));
            Assert.Equal(ConnectionStatus.Connecting, net.Status);
            Assert.Equal(0ul, net.PlayerId);
            Assert.Equal((byte)4, net.SnapshotHz);
            Assert.Equal(1, transport.DropPeerCalls);
            Assert.Equal(2, log.Debugs.Count);
        }

        [Fact]
        public void Handshake_MultipleQueuedRejections_OnlyFirstChangesReconnectState()
        {
            var transport = new MockTransport();
            var log = new RecordingLog();
            long now = 0;
            var net = new NetClient(log, transport, () => now);
            net.Connect(Options);
            transport.RaisePeerConnected();

            byte[] rejection = ServerHelloEnvelope(accepted: false, playerId: 0, snapshotHz: 0);
            transport.RaiseNetworkReceive(rejection);
            transport.RaiseNetworkReceive(rejection);
            transport.RaiseNetworkReceive(rejection);

            Assert.Equal(ConnectionStatus.Disconnected, net.Status);
            Assert.Equal(1, transport.DropPeerCalls);
            Assert.Single(log.Warnings);
            Assert.Equal(2, log.Debugs.Count);

            now = NetClient.DefaultReconnectMs;
            net.Poll();
            Assert.Equal(2, transport.ConnectCalls);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);
        }

        [Fact]
        public void ReadySession_DuplicateAcceptedHello_DoesNotReplaceIdentityOrCapabilities()
        {
            var transport = new MockTransport();
            var log = new RecordingLog();
            var net = new NetClient(log, transport, () => 0);
            net.Connect(Options);
            transport.RaisePeerConnected();
            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 77, snapshotHz: 8));

            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 99, snapshotHz: 12));

            Assert.Equal(ConnectionStatus.Ready, net.Status);
            Assert.True(net.HandshakeComplete);
            Assert.Equal(77ul, net.PlayerId);
            Assert.Equal((byte)8, net.SnapshotHz);
            Assert.Equal(0, transport.DropPeerCalls);
            Assert.Empty(log.Warnings);
            Assert.Single(log.Debugs);
        }

        [Fact]
        public void Handshake_AcceptedServerHelloWithWrongProtocol_ReturnsToDisconnected()
        {
            var transport = new MockTransport();
            var log = new RecordingLog();
            long now = 0;
            var net = new NetClient(log, transport, () => now);
            net.Connect(Options);
            transport.RaisePeerConnected();

            transport.RaiseNetworkReceive(ServerHelloEnvelope(
                accepted: true,
                playerId: 77,
                snapshotHz: 8,
                protocolVersion: NetClient.ProtocolVersion + 1));

            Assert.Equal(ConnectionStatus.Disconnected, net.Status);
            Assert.False(net.HandshakeComplete);
            Assert.Equal(1, transport.DropPeerCalls);
            Assert.False(transport.IsPeerConnected);
            Assert.Contains(log.Warnings, message => message.Contains("protocol"));
            Assert.DoesNotContain(log.Warnings, message => message.Contains(Options.Token));

            now = NetClient.DefaultReconnectMs;
            net.Poll();
            Assert.Equal(2, transport.ConnectCalls);
            Assert.Equal(2, transport.FreshPeerConnectCalls);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);

            transport.RaisePeerConnected();
            Assert.Equal(ConnectionStatus.Handshaking, net.Status);
        }

        [Fact]
        public void Handshake_AcceptedServerHelloWithoutCapabilities_ReturnsToDisconnected()
        {
            var transport = new MockTransport();
            var log = new RecordingLog();
            long now = 0;
            var net = new NetClient(log, transport, () => now);
            net.Connect(Options);
            transport.RaisePeerConnected();

            transport.RaiseNetworkReceive(ServerHelloEnvelope(
                accepted: true,
                playerId: 77,
                snapshotHz: 8,
                includeCapabilities: false));

            Assert.Equal(ConnectionStatus.Disconnected, net.Status);
            Assert.False(net.HandshakeComplete);
            Assert.Equal(1, transport.DropPeerCalls);
            Assert.False(transport.IsPeerConnected);
            Assert.Equal(
                "[Sailwind.Online] ServerHello missing capabilities; will retry.",
                Assert.Single(log.Warnings));

            now = NetClient.DefaultReconnectMs;
            net.Poll();
            Assert.Equal(2, transport.ConnectCalls);
            Assert.Equal(2, transport.FreshPeerConnectCalls);
            Assert.Equal(ConnectionStatus.Connecting, net.Status);

            transport.RaisePeerConnected();
            Assert.Equal(ConnectionStatus.Handshaking, net.Status);
        }

        [Fact]
        public void ClientHello_ResendsEveryIntervalUntilServerHello()
        {
            var transport = new MockTransport();
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);
            net.Connect(Options);

            transport.RaisePeerConnected(); // hello #1 at t=0
            Assert.Single(transport.Sent);

            now = NetClient.HelloRetryMs - 1;
            net.Poll(); // too soon
            Assert.Single(transport.Sent);

            now = NetClient.HelloRetryMs;
            net.Poll(); // hello #2
            Assert.Equal(2, transport.Sent.Count);

            now = NetClient.HelloRetryMs * 2;
            net.Poll(); // hello #3
            Assert.Equal(3, transport.Sent.Count);

            // ServerHello arrives; the resend loop must stop.
            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 1, snapshotHz: 4));
            now = NetClient.HelloRetryMs * 10;
            net.Poll();
            net.Poll();
            Assert.Equal(3, transport.Sent.Count);
        }

        [Fact]
        public void Disconnect_ReconnectsWithExponentialBackoff()
        {
            var transport = new MockTransport();
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);
            net.Connect(Options);
            transport.RaisePeerConnected(); // resets backoff to the default
            Assert.Equal(1, transport.ConnectCalls);

            // First drop: reconnect must wait DefaultReconnectMs.
            now = 0;
            transport.RaisePeerDisconnected();
            Assert.Equal(ConnectionStatus.Disconnected, net.Status);
            Assert.Equal(0, transport.DropPeerCalls);

            now = NetClient.DefaultReconnectMs - 1;
            net.Poll();
            Assert.Equal(1, transport.ConnectCalls); // still waiting

            now = NetClient.DefaultReconnectMs;
            net.Poll();
            Assert.Equal(2, transport.ConnectCalls); // reconnect fired
            Assert.Equal(ConnectionStatus.Connecting, net.Status);

            // Second drop without a successful connect in between: the gap must double.
            transport.RaisePeerDisconnected();
            long secondGap = 2 * NetClient.DefaultReconnectMs;

            now = NetClient.DefaultReconnectMs + secondGap - 1;
            net.Poll();
            Assert.Equal(2, transport.ConnectCalls); // still waiting the doubled gap

            now = NetClient.DefaultReconnectMs + secondGap;
            net.Poll();
            Assert.Equal(3, transport.ConnectCalls);
        }

        [Fact]
        public void Reconnect_BackoffIsCappedAtMax()
        {
            var transport = new MockTransport();
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);
            net.Connect(Options);

            // Keep failing to connect; the backoff climbs 1s, 2s, 4s, 8s and then holds at 8s.
            long expectedGap = NetClient.DefaultReconnectMs;
            int connects = transport.ConnectCalls;
            for (int i = 0; i < 6; i++)
            {
                transport.RaisePeerDisconnected();
                now += expectedGap;
                net.Poll();
                connects++;
                Assert.Equal(connects, transport.ConnectCalls);
                expectedGap = Math.Min(expectedGap * 2, NetClient.MaxReconnectMs);
            }
        }

        [Fact]
        public void Ping_ReflectsTransportKeepalive()
        {
            var transport = new MockTransport();
            var net = new NetClient(new NullNetLog(), transport, () => 0);

            Assert.Equal(-1, net.Ping); // no peer yet

            transport.Ping = 42;
            transport.RaisePeerConnected();
            Assert.Equal(42, net.Ping);
        }

        [Fact]
        public void Poll_WhileReady_PumpsTransportWithoutResendingHello()
        {
            var transport = new MockTransport();
            long now = 0;
            var net = new NetClient(new NullNetLog(), transport, () => now);
            net.Connect(Options);
            transport.RaisePeerConnected();
            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 1, snapshotHz: 4));
            Assert.Equal(ConnectionStatus.Ready, net.Status);

            int sentAfterHandshake = transport.Sent.Count;
            int pollsBefore = transport.PollCalls;

            now = NetClient.HelloRetryMs * 20;
            net.Poll();
            net.Poll();
            net.Poll();

            // The transport keeps being pumped (keepalive), but no ClientHello is resent once Ready.
            Assert.Equal(pollsBefore + 3, transport.PollCalls);
            Assert.Equal(sentAfterHandshake, transport.Sent.Count);
        }

        [Fact]
        public void SendClientState_OnlySendsOnceReady()
        {
            var transport = new MockTransport();
            var net = new NetClient(new NullNetLog(), transport, () => 0);
            var pose = new BoatPose();

            net.SendClientState(pose); // not connected: dropped
            Assert.Empty(transport.Sent);

            net.Connect(Options);
            transport.RaisePeerConnected(); // Handshaking (hello #1) — still not Ready
            transport.Sent.Clear();
            net.SendClientState(pose);
            Assert.Empty(transport.Sent);

            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 1, snapshotHz: 4));
            net.SendClientState(pose);

            Assert.Single(transport.Sent);
            Assert.Equal(Payload.ClientState, Decode(transport.Sent[0]).PayloadType);
        }

        [Fact]
        public void ReadySession_EncodesOutboundPoseExactly_ThenIndependentlyIngestsRemoteSnapshot()
        {
            long now = 4321;
            var transport = new MockTransport();
            var net = new NetClient(new NullNetLog(), transport, () => now);
            net.Connect(Options);
            transport.RaisePeerConnected();
            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 77, snapshotHz: 4));
            transport.Sent.Clear();

            var pose = new BoatPose(
                new Vector3(12.5f, -3.25f, 99.75f),
                new Quaternion(0.1f, 0.2f, 0.3f, 0.9f),
                new Vector3(-4.5f, 0.25f, 6.75f));
            net.SendClientState(pose);

            Assert.Single(transport.Sent);
            byte[] expected = new Codec().EncodeClientState(
                2,
                pose.Position.X, pose.Position.Y, pose.Position.Z,
                pose.Rotation.X, pose.Rotation.Y, pose.Rotation.Z, pose.Rotation.W,
                pose.Velocity.X, pose.Velocity.Y, pose.Velocity.Z,
                0,
                (uint)now);
            Assert.Equal(expected, transport.Sent[0]);
            ClientState outbound = Decode(transport.Sent[0]).PayloadAsClientState();
            Assert.Equal(pose.Position.X, outbound.Pos.Value.X);
            Assert.Equal(pose.Position.Y, outbound.Pos.Value.Y);
            Assert.Equal(pose.Position.Z, outbound.Pos.Value.Z);
            Assert.Equal(pose.Rotation.X, outbound.Rot.Value.X);
            Assert.Equal(pose.Rotation.Y, outbound.Rot.Value.Y);
            Assert.Equal(pose.Rotation.Z, outbound.Rot.Value.Z);
            Assert.Equal(pose.Rotation.W, outbound.Rot.Value.W);
            Assert.Equal(pose.Velocity.X, outbound.Vel.Value.X);
            Assert.Equal(pose.Velocity.Y, outbound.Vel.Value.Y);
            Assert.Equal(pose.Velocity.Z, outbound.Vel.Value.Z);
            Assert.Equal((uint)now, outbound.TMs);

            transport.RaiseNetworkReceive(SnapshotDeltaEnvelope(
                serverTick: 91,
                playerId: 88,
                x: -10.5f,
                y: 2.25f,
                z: 45.75f,
                rx: 0.4f,
                ry: 0.3f,
                rz: 0.2f,
                rw: 0.8f,
                aboardBoat: 123,
                tMs: 5678));

            Assert.Equal(91u, net.Cache.LastServerTick);
            Assert.True(net.Cache.TryGetPlayer(88, out var remote));
            Assert.NotNull(remote);
            Assert.Equal(-10.5f, remote.Latest.Pos.X);
            Assert.Equal(2.25f, remote.Latest.Pos.Y);
            Assert.Equal(45.75f, remote.Latest.Pos.Z);
            Assert.Equal(0.4f, remote.Latest.Rot.X);
            Assert.Equal(0.3f, remote.Latest.Rot.Y);
            Assert.Equal(0.2f, remote.Latest.Rot.Z);
            Assert.Equal(0.8f, remote.Latest.Rot.W);
            Assert.Equal(123ul, remote.Latest.Link);
            Assert.Equal(5678u, remote.Latest.TMs);
            Assert.Equal(now, remote.Latest.ReceivedMs);
        }

        [Fact]
        public void PositionObservability_LogsEachDirectionOncePerSession_AndResetsOnReconnect()
        {
            var transport = new MockTransport();
            var log = new RecordingLog();
            long now = 100;
            var net = new NetClient(log, transport, () => now);
            var pose = new BoatPose(new Vector3(1f, 2f, 3f), Quaternion.Identity, Vector3.Zero);
            byte[] snapshot = SnapshotDeltaEnvelope(1, 8, 4f, 5f, 6f, 0f, 0f, 0f, 1f, 123, 10);

            net.Connect(Options);
            net.SendClientState(pose);
            Assert.DoesNotContain(log.Infos, message => message.Contains("First outbound position"));

            transport.RaisePeerConnected();
            transport.RaiseNetworkReceive(ServerHelloEnvelope(true, 7, 4));
            net.SendClientState(pose);
            net.SendClientState(pose);
            transport.RaiseNetworkReceive(snapshot);
            transport.RaiseNetworkReceive(snapshot);

            Assert.Equal(ConnectionStatus.Ready, net.Status);
            Assert.True(Envelope.VerifyEnvelope(new ByteBuffer(snapshot)));
            Assert.Equal(8ul, Decode(snapshot).PayloadAsSnapshotDelta().Players(0).Value.PlayerId);
            string outboundLog = Assert.Single(log.Infos.FindAll(message => message.Contains("First outbound position")));
            Assert.Contains("player_id=7", outboundLog);
            Assert.Contains("pos=(1, 2, 3)", outboundLog);
            Assert.Contains("t_ms=100", outboundLog);
            Assert.True(net.Cache.TryGetPlayer(8, out _));
            string inboundLog = Assert.Single(log.Infos.FindAll(message => message.Contains("First inbound position")));
            Assert.Contains("remote_player_id=8", inboundLog);
            Assert.Contains("pos=(4, 5, 6)", inboundLog);
            Assert.Contains("t_ms=10", inboundLog);
            Assert.DoesNotContain(log.Infos, message => message.Contains(Options.Token));

            transport.RaisePeerDisconnected();
            now += NetClient.DefaultReconnectMs;
            net.Poll();
            transport.RaisePeerConnected();
            transport.RaiseNetworkReceive(ServerHelloEnvelope(true, 7, 4));
            net.SendClientState(pose);
            transport.RaiseNetworkReceive(snapshot);

            Assert.Equal(2, log.Infos.FindAll(message => message.Contains("First outbound position")).Count);
            Assert.Equal(2, log.Infos.FindAll(message => message.Contains("First inbound position")).Count);
        }

        [Fact]
        public void Receive_WorldClock_DispatchesThroughCodecIntoState()
        {
            var transport = new MockTransport();
            var net = new NetClient(new NullNetLog(), transport, () => 0);
            net.Connect(Options);
            transport.RaisePeerConnected();
            transport.RaiseNetworkReceive(ServerHelloEnvelope(accepted: true, playerId: 1, snapshotHz: 4));

            transport.RaiseNetworkReceive(WorldClockEnvelope(day: 12, timeOfDay: 0.5f));

            Assert.Equal(12u, net.ServerDay);
            Assert.Equal(0.5f, net.ServerTimeOfDay);
        }

        [Fact]
        public void Receive_GarbageDatagram_IsIgnoredWithoutThrow()
        {
            var transport = new MockTransport();
            var log = new RecordingLog();
            var net = new NetClient(log, transport, () => 0);
            net.Connect(Options);
            transport.RaisePeerConnected();

            transport.RaiseNetworkReceive(new byte[] { 1, 2, 3, 4, 5, 6, 7, 8 });

            Assert.NotEqual(ConnectionStatus.Ready, net.Status);
        }

        [Fact]
        public void NetworkError_IsSurfacedAsWarning_WithoutThrow()
        {
            // Regression: the NetworkError seam event must stay wired to a signature-compatible
            // handler. It is the one transport event no other test drives, so a mis-wired
            // Action<IPEndPoint, SocketError> handler (as in the half-migrated NetClient) would
            // slip past every other test and surface only as a production compile break.
            var transport = new MockTransport();
            var log = new RecordingLog();
            var net = new NetClient(log, transport, () => 0);
            net.Connect(Options);

            transport.RaiseNetworkError(new IPEndPoint(IPAddress.Loopback, 4242), SocketError.ConnectionReset);

            Assert.NotEmpty(log.Warnings);
            Assert.NotEqual(ConnectionStatus.Ready, net.Status);
        }

        [Fact]
        public void Dispose_StopsTransport()
        {
            var transport = new MockTransport();
            var net = new NetClient(new NullNetLog(), transport, () => 0);
            net.Connect(Options);

            net.Dispose();

            Assert.Equal(1, transport.StopCalls);
            Assert.Equal(ConnectionStatus.Disconnected, net.Status);
        }

        private static Envelope Decode(byte[] bytes)
        {
            return Envelope.GetRootAsEnvelope(new ByteBuffer(bytes));
        }

        private static byte[] ServerHelloEnvelope(
            bool accepted,
            ulong playerId,
            byte snapshotHz,
            ushort protocolVersion = NetClient.ProtocolVersion,
            bool includeCapabilities = true)
        {
            var b = new FlatBufferBuilder(128);
            StringOffset reason = b.CreateString(string.Empty);
            StringOffset serverName = b.CreateString("test-server");
            Offset<CapabilityManifest> caps = default(Offset<CapabilityManifest>);
            if (includeCapabilities)
            {
                caps = CapabilityManifest.CreateCapabilityManifest(
                    b, protocol_version: protocolVersion, snapshot_hz: snapshotHz);
            }

            Offset<ServerHello> hello = ServerHello.CreateServerHello(
                b,
                accepted: accepted,
                reasonOffset: reason,
                player_id: playerId,
                server_nameOffset: serverName,
                balance_gold: 0,
                capabilitiesOffset: caps);
            return Wrap(b, 1, Payload.ServerHello, hello.Value);
        }

        private static byte[] SnapshotDeltaEnvelope(
            uint serverTick,
            ulong playerId,
            float x,
            float y,
            float z,
            float rx,
            float ry,
            float rz,
            float rw,
            ulong aboardBoat,
            uint tMs)
        {
            var b = new FlatBufferBuilder(256);
            PlayerState.StartPlayerState(b);
            PlayerState.AddPlayerId(b, playerId);
            PlayerState.AddPos(b, Vec3.CreateVec3(b, x, y, z));
            PlayerState.AddRot(b, QuatC.CreateQuatC(b, rx, ry, rz, rw));
            PlayerState.AddAboardBoat(b, aboardBoat);
            PlayerState.AddTMs(b, tMs);
            Offset<PlayerState> player = PlayerState.EndPlayerState(b);
            VectorOffset players = SnapshotDelta.CreatePlayersVector(b, new[] { player });
            Offset<SnapshotDelta> delta = SnapshotDelta.CreateSnapshotDelta(
                b,
                server_tick: serverTick,
                playersOffset: players);
            return Wrap(b, 3, Payload.SnapshotDelta, delta.Value);
        }

        private static byte[] WorldClockEnvelope(uint day, float timeOfDay)
        {
            var b = new FlatBufferBuilder(64);

            // Populate every WorldClock field, including moon_phase, so the union payload table is
            // fully written. The pinned FlatBuffers 25.2.10 verifier that Codec.TryDispatch runs
            // rejects a partially-populated union payload table, so a clock that left moon_phase at
            // its 0 default would be dropped before dispatch and never reach NetClient. This mirrors
            // the fully-populated clock in CodecTests; day and time_of_day are the values asserted.
            Offset<WorldClock> clock = WorldClock.CreateWorldClock(b, day: day, time_of_day: timeOfDay, moon_phase: 0.25f);
            return Wrap(b, 2, Payload.WorldClock, clock.Value);
        }

        private static byte[] Wrap(FlatBufferBuilder b, uint seq, Payload type, int payloadOffset)
        {
            Offset<Envelope> env = Envelope.CreateEnvelope(b, seq, type, payloadOffset);
            Envelope.FinishEnvelopeBuffer(b, env);
            return b.SizedByteArray();
        }

        private sealed class NullNetLog : INetLog
        {
            public void LogDebug(string message) { }
            public void LogInfo(string message) { }
            public void LogWarning(string message) { }
            public void LogError(string message) { }
        }

        private sealed class RecordingLog : INetLog
        {
            public readonly List<string> Debugs = new List<string>();
            public readonly List<string> Errors = new List<string>();
            public readonly List<string> Infos = new List<string>();
            public readonly List<string> Warnings = new List<string>();

            public void LogDebug(string message) { Debugs.Add(message); }
            public void LogInfo(string message) { Infos.Add(message); }
            public void LogWarning(string message) { Warnings.Add(message); }
            public void LogError(string message) { Errors.Add(message); }
        }
    }
}
