using System;
using System.Diagnostics;
using System.Net;
using System.Net.Sockets;
using LiteNetLib;
using Sailwind.Api;
using Sailwind.Online.Client.Sync;
using SwProto;

namespace Sailwind.Online.Client.Net
{
    /// <summary>Connection lifecycle as seen by the rest of the plugin.</summary>
    public enum ConnectionStatus
    {
        Disconnected,
        Connecting,
        Handshaking,
        Ready
    }

    /// <summary>Everything needed to open (and re-open) a session.</summary>
    public sealed class ConnectOptions
    {
        public string Host = "127.0.0.1";
        public int Port = 38455;
        public string DisplayName = string.Empty;
        public string Token = string.Empty;
        public string GameBuild = string.Empty;
        public string ModVersion = string.Empty;
        public string ApiSurfaceHash = string.Empty;
    }

    /// <summary>
    /// The client half of the wire protocol. Wraps a LiteNetLib 1.3.1 <see cref="NetManager"/>,
    /// drives the app-level ClientHello retry, and turns received envelopes into cache updates.
    /// Polled manually from the Unity main thread, so every listener callback runs there too.
    /// Init-0 rule: every send uses <see cref="DeliveryMethod.Unreliable"/> — sw-net has no
    /// reliable channel yet, so hello reliability is the 250 ms resend loop below.
    /// </summary>
    public sealed class NetClient : IServerMessageHandler, IClientStateSender, IDisposable
    {
        public const ushort ProtocolVersion = 1;
        public const string ConnectKey = "sailwind-online";
        public const int Mtu = 1024;
        public const int DisconnectTimeoutMs = 5000;
        public const long HelloRetryMs = 250;
        public const long DefaultReconnectMs = 1000;
        public const long MaxReconnectMs = 8000;

        private readonly INetLog _log;
        private readonly Codec _codec = new Codec();
        private readonly SnapshotCache _cache = new SnapshotCache();
        private readonly ITransport _transport;
        private readonly Stopwatch _clock = Stopwatch.StartNew();
        private readonly Func<long> _nowMs;

        private ConnectOptions? _options;
        private ConnectionStatus _status = ConnectionStatus.Disconnected;
        private uint _seq;

        private long _lastHelloMs;
        private long _nextReconnectMs;
        private long _reconnectBackoffMs = DefaultReconnectMs;

        private byte _snapshotHz = 4;
        private ulong _playerId;
        private uint _serverDay;
        private float _serverTimeOfDay;

        /// <summary>Production constructor: drives the real LiteNetLib 1.3.1 transport.</summary>
        public NetClient(INetLog log)
            : this(log, new LiteNetLibTransport(), null)
        {
        }

        /// <summary>
        /// Seam constructor: inject an <see cref="ITransport"/> (and optional monotonic clock) so the
        /// session state machine can be unit-tested against a fake. <paramref name="nowMs"/> defaults
        /// to the internal <see cref="Stopwatch"/>, so production timing is unchanged.
        /// </summary>
        public NetClient(INetLog log, ITransport transport, Func<long>? nowMs = null)
        {
            _log = log;
            _transport = transport;
            _nowMs = nowMs ?? (() => _clock.ElapsedMilliseconds);

            _transport.PeerConnected += OnPeerConnected;
            _transport.PeerDisconnected += OnPeerDisconnected;
            _transport.NetworkReceive += OnNetworkReceive;
            _transport.NetworkError += OnNetworkError;
        }

        public SnapshotCache Cache
        {
            get { return _cache; }
        }

        public ConnectionStatus Status
        {
            get { return _status; }
        }

        public bool HandshakeComplete
        {
            get { return _status == ConnectionStatus.Ready; }
        }

        /// <summary>Snapshot rate advertised by the server (Hz); drives <c>StateReporter</c>.</summary>
        public byte SnapshotHz
        {
            get { return _snapshotHz; }
        }

        public ulong PlayerId
        {
            get { return _playerId; }
        }

        public uint ServerDay
        {
            get { return _serverDay; }
        }

        public float ServerTimeOfDay
        {
            get { return _serverTimeOfDay; }
        }

        /// <summary>Current round-trip estimate in milliseconds, or -1 when not connected.</summary>
        public int Ping
        {
            get { return _transport.Ping; }
        }

        /// <summary>Monotonic millisecond clock shared by all timing in the plugin.</summary>
        public long NowMs
        {
            get { return _nowMs(); }
        }

        public string StatusText
        {
            get
            {
                switch (_status)
                {
                    case ConnectionStatus.Connecting:
                        return "connecting";
                    case ConnectionStatus.Handshaking:
                        return "handshaking";
                    case ConnectionStatus.Ready:
                        return "connected";
                    default:
                        return "disconnected";
                }
            }
        }

        /// <summary>Begin (or restart) a session with the given options.</summary>
        public void Connect(ConnectOptions options)
        {
            _options = options;

            if (!_transport.IsRunning && !_transport.Start())
            {
                _log.LogError("[Sailwind.Online] Failed to start LiteNetLib NetManager.");
                return;
            }

            OpenPeer();
        }

        /// <summary>Poll the transport and service the handshake/reconnect timers. Call every frame.</summary>
        public void Poll()
        {
            if (_transport.IsRunning)
            {
                _transport.PollEvents();
            }

            long now = NowMs;

            if (_status == ConnectionStatus.Handshaking && now - _lastHelloMs >= HelloRetryMs)
            {
                SendHello();
            }
            else if (_status == ConnectionStatus.Disconnected && _options != null && now >= _nextReconnectMs)
            {
                OpenPeer();
            }

            _cache.PruneStale(now, SnapshotCache.DefaultStaleMs);
        }

        /// <summary>Report the local boat's absolute-world pose to the server as a ClientState.</summary>
        public void SendClientState(BoatPose pose)
        {
            if (!HandshakeComplete)
            {
                return;
            }

            byte[] bytes = _codec.EncodeClientState(
                NextSeq(),
                pose.Position.X, pose.Position.Y, pose.Position.Z,
                pose.Rotation.X, pose.Rotation.Y, pose.Rotation.Z, pose.Rotation.W,
                pose.Velocity.X, pose.Velocity.Y, pose.Velocity.Z,
                0UL,
                unchecked((uint)NowMs));

            SendRaw(bytes);
        }

        public void Dispose()
        {
            _transport.Stop();
            _status = ConnectionStatus.Disconnected;
        }

        private void OpenPeer()
        {
            ConnectOptions? options = _options;
            if (options == null)
            {
                return;
            }

            _cache.Clear();
            _transport.Connect(options.Host, options.Port, ConnectKey);
            _status = ConnectionStatus.Connecting;
            _log.LogInfo("[Sailwind.Online] Connecting to " + options.Host + ":" + options.Port + " ...");
        }

        private void ScheduleReconnect()
        {
            _nextReconnectMs = NowMs + _reconnectBackoffMs;
            _reconnectBackoffMs = Math.Min(_reconnectBackoffMs * 2, MaxReconnectMs);
        }

        private uint NextSeq()
        {
            _seq++;
            return _seq;
        }

        private void SendHello()
        {
            ConnectOptions? options = _options;
            if (options == null)
            {
                return;
            }

            byte[] bytes = _codec.EncodeClientHello(
                NextSeq(),
                ProtocolVersion,
                options.DisplayName,
                options.Token,
                options.GameBuild,
                options.ModVersion,
                options.ApiSurfaceHash);

            SendRaw(bytes);
            _lastHelloMs = NowMs;
        }

        private void SendRaw(byte[] bytes)
        {
            if (!_transport.IsPeerConnected)
            {
                return;
            }

            if (bytes.Length > Mtu)
            {
                _log.LogWarning("[Sailwind.Online] Dropping oversized packet (" + bytes.Length + " > " + Mtu + " bytes).");
                return;
            }

            _transport.Send(bytes, DeliveryMethod.Unreliable);
        }

        private void OnPeerConnected()
        {
            _status = ConnectionStatus.Handshaking;
            _reconnectBackoffMs = DefaultReconnectMs;
            SendHello();
            _log.LogInfo("[Sailwind.Online] Transport up; sending ClientHello.");
        }

        private void OnPeerDisconnected(string reason)
        {
            _status = ConnectionStatus.Disconnected;
            _cache.Clear();
            ScheduleReconnect();
            _log.LogInfo("[Sailwind.Online] Disconnected (" + reason + "); will retry.");
        }

        private void OnNetworkReceive(byte[] data)
        {
            if (!_codec.TryDispatch(data, this))
            {
                _log.LogDebug("[Sailwind.Online] Ignored malformed datagram (" + data.Length + " bytes).");
            }
        }

        private void OnNetworkError(IPEndPoint endPoint, SocketError socketError)
        {
            _log.LogWarning("[Sailwind.Online] Socket error from " + endPoint + ": " + socketError);
        }

        void IServerMessageHandler.OnServerHello(ServerHello hello, uint seq)
        {
            if (!hello.Accepted)
            {
                _status = ConnectionStatus.Disconnected;
                ScheduleReconnect();
                _log.LogWarning("[Sailwind.Online] ServerHello rejected: " + (hello.Reason ?? "no reason"));
                return;
            }

            _status = ConnectionStatus.Ready;
            _playerId = hello.PlayerId;
            _reconnectBackoffMs = DefaultReconnectMs;

            CapabilityManifest? caps = hello.Capabilities;
            if (caps.HasValue && caps.Value.SnapshotHz > 0)
            {
                _snapshotHz = caps.Value.SnapshotHz;
            }

            ApplyClock(hello.Clock);

            _log.LogInfo(
                "[Sailwind.Online] Connected to " + _options?.Host + ":" + _options?.Port +
                " as player " + _playerId +
                " — day " + _serverDay + ", " + FormatTimeOfDay(_serverTimeOfDay));
        }

        void IServerMessageHandler.OnSnapshotDelta(SnapshotDelta delta, uint seq)
        {
            _cache.ObserveServerTick(delta.ServerTick);
            long now = NowMs;

            int playerCount = delta.PlayersLength;
            for (int i = 0; i < playerCount; i++)
            {
                PlayerState? ps = delta.Players(i);
                if (ps.HasValue)
                {
                    IngestPlayer(ps.Value, now);
                }
            }

            int boatCount = delta.BoatsLength;
            for (int i = 0; i < boatCount; i++)
            {
                BoatState? bs = delta.Boats(i);
                if (bs.HasValue)
                {
                    IngestBoat(bs.Value, now);
                }
            }
        }

        void IServerMessageHandler.OnCellSnapshot(CellSnapshot snapshot, uint seq)
        {
            long now = NowMs;

            int playerCount = snapshot.PlayersLength;
            for (int i = 0; i < playerCount; i++)
            {
                PlayerState? ps = snapshot.Players(i);
                if (ps.HasValue)
                {
                    IngestPlayer(ps.Value, now);
                }
            }

            int boatCount = snapshot.BoatsLength;
            for (int i = 0; i < boatCount; i++)
            {
                BoatState? bs = snapshot.Boats(i);
                if (bs.HasValue)
                {
                    IngestBoat(bs.Value, now);
                }
            }
        }

        void IServerMessageHandler.OnAoiUpdate(AoiUpdate update, uint seq)
        {
            // Area-of-interest cell membership informs remote-entity rendering, which is a later
            // milestone. At init-0 stale entities fall out of the cache by timeout instead.
            _log.LogDebug("[Sailwind.Online] AoI update: +" + update.AddedLength + " / -" + update.RemovedLength + " cells.");
        }

        void IServerMessageHandler.OnWorldClock(WorldClock clock, uint seq)
        {
            _serverDay = clock.Day;
            _serverTimeOfDay = clock.TimeOfDay;
        }

        void IServerMessageHandler.OnUnhandled(Payload payloadType, uint seq)
        {
            // Moorage, ledger and chat payloads decode correctly but their client-side handling
            // (moorage UI, economy hooks, chat UI) is deferred; log and drop rather than stub.
            _log.LogDebug("[Sailwind.Online] Received unhandled payload " + payloadType + " (seq " + seq + ").");
        }

        private void ApplyClock(WorldClock? clock)
        {
            if (clock.HasValue)
            {
                _serverDay = clock.Value.Day;
                _serverTimeOfDay = clock.Value.TimeOfDay;
            }
        }

        private void IngestPlayer(PlayerState ps, long now)
        {
            EntitySample sample = new EntitySample
            {
                Pos = ToVec3(ps.Pos),
                Rot = ToQuat(ps.Rot),
                Vel = default(NetVec3),
                TMs = ps.TMs,
                ReceivedMs = now,
                Link = ps.AboardBoat
            };
            _cache.UpsertPlayer(ps.PlayerId, sample);
        }

        private void IngestBoat(BoatState bs, long now)
        {
            EntitySample sample = new EntitySample
            {
                Pos = ToVec3(bs.Pos),
                Rot = ToQuat(bs.Rot),
                Vel = ToVec3(bs.Vel),
                TMs = bs.TMs,
                ReceivedMs = now,
                Link = bs.Owner
            };
            _cache.UpsertBoat(bs.BoatId, sample);
        }

        private static NetVec3 ToVec3(Vec3? v)
        {
            if (!v.HasValue)
            {
                return default(NetVec3);
            }

            Vec3 value = v.Value;
            return new NetVec3(value.X, value.Y, value.Z);
        }

        private static NetQuat ToQuat(QuatC? q)
        {
            if (!q.HasValue)
            {
                return new NetQuat(0f, 0f, 0f, 1f);
            }

            QuatC value = q.Value;
            return new NetQuat(value.X, value.Y, value.Z, value.W);
        }

        public static string FormatTimeOfDay(float fractionOfDay)
        {
            float clamped = fractionOfDay - (float)Math.Floor(fractionOfDay);
            int totalMinutes = (int)(clamped * 24f * 60f);
            int hours = (totalMinutes / 60) % 24;
            int minutes = totalMinutes % 60;
            return hours.ToString("00") + ":" + minutes.ToString("00");
        }
    }
}
