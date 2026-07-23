using System;
using System.Collections.Generic;
using System.Net;
using System.Net.Sockets;
using LiteNetLib;
using Sailwind.Online.Client.Net;

namespace Sailwind.Online.Net.Tests
{
    /// <summary>
    /// In-memory <see cref="ITransport"/> double. It records the operations <see cref="NetClient"/>
    /// performs (start/stop/connect/poll/send) and lets a test drive the peer lifecycle by raising
    /// the transport events by hand, so the session state machine can be exercised with zero
    /// sockets and fully deterministic timing.
    /// </summary>
    internal sealed class MockTransport : ITransport
    {
        /// <summary>What <see cref="Start"/> returns; false simulates a socket that cannot bind.</summary>
        public bool StartResult = true;

        /// <summary>Queued <see cref="Start"/> outcomes, consumed before <see cref="StartResult"/>.</summary>
        public readonly Queue<bool> StartResults = new Queue<bool>();

        /// <summary>What <see cref="Connect"/> reports; false simulates no peer being created.</summary>
        public bool ConnectResult = true;

        public int StartCalls;
        public int StopCalls;
        public int ConnectCalls;
        public int FreshPeerConnectCalls;
        public int DropPeerCalls;
        public int PollCalls;

        /// <summary>Optional callback invoked during <see cref="DropPeer"/> to model queued callbacks.</summary>
        public Action DropPeerCallback;

        public string LastHost;
        public int LastPort;
        public string LastKey;

        /// <summary>Every datagram NetClient handed to <see cref="Send"/>, in order.</summary>
        public readonly List<byte[]> Sent = new List<byte[]>();

        /// <summary>Queued send failures, consumed before a datagram is recorded.</summary>
        public readonly Queue<Exception> SendFailures = new Queue<Exception>();

        private bool _running;
        private bool _hasPeer;
        private bool _peerConnected;

        public bool IsRunning => _running;

        public bool IsPeerConnected => _peerConnected;

        public int MaxUnreliablePayloadSize { get; set; } = NetClient.Mtu - 1;

        public int Ping { get; set; } = -1;

        public event Action PeerConnected;
        public event Action<string> PeerDisconnected;
        public event Action<byte[]> NetworkReceive;
        public event Action<IPEndPoint, SocketError> NetworkError;

        public bool Start()
        {
            StartCalls++;
            bool result = StartResults.Count > 0 ? StartResults.Dequeue() : StartResult;
            if (result)
            {
                _running = true;
            }

            return result;
        }

        public bool Connect(string host, int port, string key)
        {
            ConnectCalls++;
            if (ConnectResult && !_hasPeer)
            {
                FreshPeerConnectCalls++;
            }

            if (ConnectResult)
            {
                _hasPeer = true;
            }

            LastHost = host;
            LastPort = port;
            LastKey = key;
            return _hasPeer;
        }

        public void DropPeer()
        {
            DropPeerCalls++;
            _hasPeer = false;
            _peerConnected = false;
            Ping = -1;
            DropPeerCallback?.Invoke();
        }

        public void Send(byte[] data, DeliveryMethod deliveryMethod)
        {
            if (SendFailures.Count > 0)
            {
                throw SendFailures.Dequeue();
            }

            Sent.Add(data);
        }

        public void PollEvents()
        {
            PollCalls++;
        }

        public void Stop()
        {
            StopCalls++;
            _running = false;
            _hasPeer = false;
            _peerConnected = false;
        }

        // --- test drivers: raise the transport events the way real LiteNetLib would ---

        /// <summary>Simulate the transport completing its connect: the peer becomes send-ready first.</summary>
        public void RaisePeerConnected()
        {
            _hasPeer = true;
            _peerConnected = true;
            PeerConnected?.Invoke();
        }

        /// <summary>Simulate the peer dropping; the peer is no longer send-ready.</summary>
        public void RaisePeerDisconnected(string reason = "RemoteConnectionClose")
        {
            _hasPeer = false;
            _peerConnected = false;
            PeerDisconnected?.Invoke(reason);
        }

        /// <summary>Deliver one received datagram to NetClient.</summary>
        public void RaiseNetworkReceive(byte[] data)
        {
            NetworkReceive?.Invoke(data);
        }

        /// <summary>Deliver a socket error to NetClient.</summary>
        public void RaiseNetworkError(IPEndPoint endPoint, SocketError error)
        {
            NetworkError?.Invoke(endPoint, error);
        }
    }
}
