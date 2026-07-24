using System;
using System.Threading;
using LiteNetLib;
using Sailwind.Online.Client.Net;
using Xunit;

namespace Sailwind.Online.Net.Tests
{
    public sealed class LiteNetLibTransportTests
    {
        private const string ConnectKey = "transport-capacity-test";

        [Fact]
        public void MaxUnreliablePayloadSize_MatchesConnectedLiteNetLibPeerCapacity()
        {
            var serverListener = new EventBasedNetListener();
            var server = new NetManager(serverListener);
            var transport = new LiteNetLibTransport();
            NetPeer serverPeer = null;

            serverListener.ConnectionRequestEvent += request => request.AcceptIfKey(ConnectKey);
            serverListener.PeerConnectedEvent += peer => serverPeer = peer;

            try
            {
                Assert.True(server.Start(0));
                Assert.True(transport.Start());
                Assert.True(transport.Connect("127.0.0.1", server.LocalPort, ConnectKey));
                Assert.True(
                    SpinWait.SpinUntil(
                        () =>
                        {
                            server.PollEvents();
                            transport.PollEvents();
                            return transport.IsPeerConnected && serverPeer != null;
                        },
                        TimeSpan.FromSeconds(5)),
                    "the real loopback LiteNetLib peers did not connect");

                int peerCapacity = serverPeer!.GetMaxSinglePacketSize(DeliveryMethod.Unreliable);

                Assert.Equal(peerCapacity, transport.MaxUnreliablePayloadSize);
                Assert.InRange(transport.MaxUnreliablePayloadSize, 1, NetClient.Mtu - 1);
            }
            finally
            {
                transport.Stop();
                if (server.IsRunning)
                {
                    server.Stop();
                }
            }
        }
    }
}
