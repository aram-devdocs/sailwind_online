// A single LiteNetLib 1.3.1 client peer used by the conformance checks.
//
// This is the REAL client library, not a reimplementation: the point of the
// harness is that if genuine LiteNetLib interoperates with the Rust sw-net
// server, the game client will too. All init-0 sends are Unreliable because
// sw-net has no reliable channel yet (milestone M1); application reliability
// (hello, econ, moorage) is app-level retry plus server idempotency.

using System;
using System.Collections.Concurrent;
using LiteNetLib;

namespace Sailwind.ProtocolSmoke
{
    internal sealed class SmokeClient : IDisposable
    {
        public const string ConnectKey = "sailwind-online";

        private readonly EventBasedNetListener _listener;
        private readonly NetManager _manager;
        private readonly ConcurrentQueue<byte[]> _received = new ConcurrentQueue<byte[]>();

        private NetPeer _peer;
        private volatile bool _connected;
        private volatile bool _disconnected;

        public SmokeClient(string name)
        {
            Name = name;
            _listener = new EventBasedNetListener();
            _manager = new NetManager(_listener)
            {
                // The harness pumps events manually via Poll(); keep the socket
                // buffers small and MTU fixed at the sw-net init-0 subset.
                UnsyncedEvents = false,
                DisconnectTimeout = 5000,
            };

            _listener.PeerConnectedEvent += peer =>
            {
                _peer = peer;
                _connected = true;
            };
            _listener.PeerDisconnectedEvent += (peer, info) =>
            {
                _connected = false;
                _disconnected = true;
            };
            _listener.NetworkReceiveEvent += (peer, reader, channel, method) =>
            {
                _received.Enqueue(reader.GetRemainingBytes());
                reader.Recycle();
            };

            if (!_manager.Start())
            {
                throw new InvalidOperationException($"LiteNetLib NetManager failed to start for client '{name}'.");
            }
        }

        public string Name { get; }

        public bool Connected => _connected;

        public bool Disconnected => _disconnected;

        public void Connect(string host, int port)
        {
            _peer = _manager.Connect(host, port, ConnectKey);
        }

        // Dispatches queued LiteNetLib events onto the calling thread and keeps
        // the connection alive (ping/pong, timeout tracking).
        public void Poll()
        {
            _manager.PollEvents();
        }

        public void Send(byte[] bytes)
        {
            var peer = _peer;
            if (peer != null && peer.ConnectionState == ConnectionState.Connected)
            {
                peer.Send(bytes, DeliveryMethod.Unreliable);
            }
        }

        public bool TryReceive(out byte[] data)
        {
            return _received.TryDequeue(out data);
        }

        public void Disconnect()
        {
            _manager.DisconnectAll();
            _manager.PollEvents();
        }

        public void Dispose()
        {
            _manager.Stop();
        }
    }
}
