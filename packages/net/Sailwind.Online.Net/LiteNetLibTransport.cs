using System;
using System.Net;
using System.Net.Sockets;
using LiteNetLib;

namespace Sailwind.Online.Client.Net
{
    /// <summary>
    /// The production <see cref="ITransport"/>: a LiteNetLib 1.3.1 <see cref="NetManager"/> driving a
    /// single client peer. This is the only place the concrete networking library is touched — the
    /// <see cref="NetManager"/>/<see cref="EventBasedNetListener"/> configuration and the peer
    /// bookkeeping moved here verbatim from <see cref="NetClient"/>, so the wire behavior is
    /// unchanged; the seam exists purely to make the session state machine unit-testable.
    /// </summary>
    public sealed class LiteNetLibTransport : ITransport
    {
        private readonly EventBasedNetListener _listener = new EventBasedNetListener();
        private readonly NetManager _manager;
        private NetPeer? _peer;
        private NetPeer? _locallyDroppedPeer;

        public LiteNetLibTransport()
        {
            _manager = new NetManager(_listener)
            {
                UnsyncedEvents = false,
                AutoRecycle = false,
                DisconnectTimeout = NetClient.DisconnectTimeoutMs
            };

            _listener.PeerConnectedEvent += OnPeerConnected;
            _listener.PeerDisconnectedEvent += OnPeerDisconnected;
            _listener.NetworkReceiveEvent += OnNetworkReceive;
            _listener.NetworkErrorEvent += OnNetworkError;
        }

        public bool IsRunning
        {
            get { return _manager.IsRunning; }
        }

        public bool IsPeerConnected
        {
            get { return _peer != null && _peer.ConnectionState == ConnectionState.Connected; }
        }

        public int Ping
        {
            get { return _peer != null ? _peer.Ping : -1; }
        }

        public event Action? PeerConnected;
        public event Action<string>? PeerDisconnected;
        public event Action<byte[]>? NetworkReceive;
        public event Action<IPEndPoint, SocketError>? NetworkError;

        public bool Start()
        {
            return _manager.Start();
        }

        public void Connect(string host, int port, string key)
        {
            _peer = _manager.Connect(host, port, key);
        }

        public void DropPeer()
        {
            NetPeer? peer = _peer;
            if (peer == null)
            {
                return;
            }

            _locallyDroppedPeer = peer;
            _peer = null;
            _manager.DisconnectPeerForce(peer);
        }

        public void Send(byte[] data, DeliveryMethod deliveryMethod)
        {
            _peer?.Send(data, deliveryMethod);
        }

        public void PollEvents()
        {
            _manager.PollEvents();
        }

        public void Stop()
        {
            if (_manager.IsRunning)
            {
                _manager.Stop();
            }

            _peer = null;
        }

        private void OnPeerConnected(NetPeer peer)
        {
            _peer = peer;
            PeerConnected?.Invoke();
        }

        private void OnPeerDisconnected(NetPeer peer, DisconnectInfo info)
        {
            if (ReferenceEquals(_locallyDroppedPeer, peer))
            {
                _locallyDroppedPeer = null;
                return;
            }

            if (ReferenceEquals(_peer, peer))
            {
                _peer = null;
            }

            PeerDisconnected?.Invoke(info.Reason.ToString());
        }

        private void OnNetworkReceive(NetPeer peer, NetPacketReader reader, byte channelNumber, DeliveryMethod deliveryMethod)
        {
            byte[] data = reader.GetRemainingBytes();
            reader.Recycle();
            NetworkReceive?.Invoke(data);
        }

        private void OnNetworkError(IPEndPoint endPoint, SocketError socketError)
        {
            NetworkError?.Invoke(endPoint, socketError);
        }
    }
}
