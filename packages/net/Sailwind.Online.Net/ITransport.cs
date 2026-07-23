using System;
using System.Net;
using System.Net.Sockets;
using LiteNetLib;

namespace Sailwind.Online.Client.Net
{
    /// <summary>
    /// The exact LiteNetLib surface <see cref="NetClient"/> drives, behind a seam so the session
    /// state machine (connect, ClientHello retry, reconnect backoff, send/receive) can be
    /// unit-tested against a fake with zero sockets. The implementation owns the single client
    /// peer; <see cref="NetClient"/> speaks only in terms of "is the manager up", "is the peer
    /// send-ready", "send these bytes", and the three lifecycle events below.
    /// </summary>
    public interface ITransport
    {
        /// <summary>True once <see cref="Start"/> has brought the manager up.</summary>
        bool IsRunning { get; }

        /// <summary>True while the current peer is fully connected, i.e. safe to send on.</summary>
        bool IsPeerConnected { get; }

        /// <summary>
        /// Maximum payload bytes the current peer can send as one unreliable packet, or zero when
        /// there is no current peer.
        /// </summary>
        int MaxUnreliablePayloadSize { get; }

        /// <summary>Round-trip estimate in milliseconds for the current peer, or -1 when there is none.</summary>
        int Ping { get; }

        /// <summary>Bring the manager up. Returns false when the socket cannot bind.</summary>
        bool Start();

        /// <summary>
        /// Open (or keep opening) the single peer to <paramref name="host"/>:<paramref name="port"/>
        /// with the connect key. Returns true when a current peer exists after the attempt.
        /// </summary>
        bool Connect(string host, int port, string key);

        /// <summary>Immediately drop the current peer without reporting a network-originated disconnect.</summary>
        void DropPeer();

        /// <summary>Send one datagram to the current peer with the given delivery method.</summary>
        void Send(byte[] data, DeliveryMethod deliveryMethod);

        /// <summary>Pump queued socket events; the events below are raised on the caller's thread.</summary>
        void PollEvents();

        /// <summary>Bring the manager down and drop the peer.</summary>
        void Stop();

        /// <summary>Raised when the peer completes its transport-level connect and becomes send-ready.</summary>
        event Action PeerConnected;

        /// <summary>Raised when the peer drops; the argument is the disconnect reason text (log only).</summary>
        event Action<string> PeerDisconnected;

        /// <summary>Raised for each received datagram, carrying the payload bytes only.</summary>
        event Action<byte[]> NetworkReceive;

        /// <summary>Raised on a socket error; endpoint and error are for logging only.</summary>
        event Action<IPEndPoint, SocketError> NetworkError;
    }
}
