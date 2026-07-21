using System.Collections.Generic;

namespace Sailwind.Online.Client.Sync
{
    /// <summary>Minimal position/rotation triple, decoupled from UnityEngine so the cache is pure C#.</summary>
    public struct NetVec3
    {
        public float X;
        public float Y;
        public float Z;

        public NetVec3(float x, float y, float z)
        {
            X = x;
            Y = y;
            Z = z;
        }
    }

    /// <summary>Minimal quaternion, decoupled from UnityEngine so the cache is pure C#.</summary>
    public struct NetQuat
    {
        public float X;
        public float Y;
        public float Z;
        public float W;

        public NetQuat(float x, float y, float z, float w)
        {
            X = x;
            Y = y;
            Z = z;
            W = w;
        }
    }

    /// <summary>One received transform sample for a remote entity.</summary>
    public struct EntitySample
    {
        /// <summary>Absolute world position.</summary>
        public NetVec3 Pos;
        /// <summary>World rotation.</summary>
        public NetQuat Rot;
        /// <summary>Linear velocity (zero for players).</summary>
        public NetVec3 Vel;
        /// <summary>Entity-authored timestamp from the wire (ms).</summary>
        public uint TMs;
        /// <summary>Local receive time (ms), used for staleness pruning and later interpolation.</summary>
        public long ReceivedMs;
        /// <summary>For a player: the boat it is aboard (0 = none). For a boat: its owner player id.</summary>
        public ulong Link;
    }

    /// <summary>
    /// Latest sample plus a fixed-capacity timestamped ring buffer for one remote entity.
    /// The ring is shaped for snapshot interpolation, which is a later milestone; at init-0
    /// only <see cref="Latest"/> and the counts are consumed (by the HUD).
    /// </summary>
    public sealed class EntityTrack
    {
        public readonly ulong Id;
        private readonly EntitySample[] _ring;
        private int _head;
        private int _count;

        public EntityTrack(ulong id, int capacity)
        {
            Id = id;
            _ring = new EntitySample[capacity];
        }

        /// <summary>Most recently received sample.</summary>
        public EntitySample Latest { get; private set; }

        /// <summary>Local time (ms) of the most recent update.</summary>
        public long LastUpdatedMs { get; private set; }

        /// <summary>Number of samples currently retained in the ring (up to capacity).</summary>
        public int SampleCount
        {
            get { return _count; }
        }

        public void Push(in EntitySample sample)
        {
            Latest = sample;
            LastUpdatedMs = sample.ReceivedMs;
            _ring[_head] = sample;
            _head = (_head + 1) % _ring.Length;
            if (_count < _ring.Length)
            {
                _count++;
            }
        }

        /// <summary>
        /// Retrieve a retained sample, index 0 = oldest .. SampleCount-1 = newest.
        /// Returns false when the index is out of range.
        /// </summary>
        public bool TryGetSample(int indexFromOldest, out EntitySample sample)
        {
            if (indexFromOldest < 0 || indexFromOldest >= _count)
            {
                sample = default(EntitySample);
                return false;
            }

            int start = (_head - _count + _ring.Length) % _ring.Length;
            int idx = (start + indexFromOldest) % _ring.Length;
            sample = _ring[idx];
            return true;
        }
    }

    /// <summary>
    /// Holds the latest known state of every remote player and boat the server has told us about.
    /// Pure data structure with no UnityEngine or FlatBuffers dependency, so it is unit-testable
    /// in a game-free process. There is no in-world rendering of these entities at init-0.
    /// </summary>
    public sealed class SnapshotCache
    {
        public const int RingCapacity = 32;
        public const long DefaultStaleMs = 5000;

        private readonly Dictionary<ulong, EntityTrack> _players = new Dictionary<ulong, EntityTrack>();
        private readonly Dictionary<ulong, EntityTrack> _boats = new Dictionary<ulong, EntityTrack>();
        private readonly List<ulong> _removeScratch = new List<ulong>();

        /// <summary>Highest server tick observed across applied snapshots.</summary>
        public uint LastServerTick { get; private set; }

        public int PlayerCount
        {
            get { return _players.Count; }
        }

        public int BoatCount
        {
            get { return _boats.Count; }
        }

        public IEnumerable<EntityTrack> Players
        {
            get { return _players.Values; }
        }

        public IEnumerable<EntityTrack> Boats
        {
            get { return _boats.Values; }
        }

        /// <summary>Advance the observed server tick monotonically (older/out-of-order ticks are ignored).</summary>
        public void ObserveServerTick(uint tick)
        {
            if (tick >= LastServerTick)
            {
                LastServerTick = tick;
            }
        }

        public void UpsertPlayer(ulong id, in EntitySample sample)
        {
            Upsert(_players, id, sample);
        }

        public void UpsertBoat(ulong id, in EntitySample sample)
        {
            Upsert(_boats, id, sample);
        }

        public bool TryGetPlayer(ulong id, out EntityTrack track)
        {
            return _players.TryGetValue(id, out track);
        }

        public bool TryGetBoat(ulong id, out EntityTrack track)
        {
            return _boats.TryGetValue(id, out track);
        }

        /// <summary>
        /// Drop entities not updated within <paramref name="maxAgeMs"/> of <paramref name="nowMs"/>.
        /// This is how remotes that leave our area of interest (or disconnect) fall out of the counts,
        /// since init-0 has no per-entity removal message. Returns the number of entities removed.
        /// </summary>
        public int PruneStale(long nowMs, long maxAgeMs)
        {
            return Prune(_players, nowMs, maxAgeMs) + Prune(_boats, nowMs, maxAgeMs);
        }

        public void Clear()
        {
            _players.Clear();
            _boats.Clear();
            LastServerTick = 0;
        }

        private static void Upsert(Dictionary<ulong, EntityTrack> map, ulong id, in EntitySample sample)
        {
            EntityTrack track;
            if (!map.TryGetValue(id, out track))
            {
                track = new EntityTrack(id, RingCapacity);
                map[id] = track;
            }

            track.Push(sample);
        }

        private int Prune(Dictionary<ulong, EntityTrack> map, long nowMs, long maxAgeMs)
        {
            _removeScratch.Clear();
            foreach (KeyValuePair<ulong, EntityTrack> entry in map)
            {
                if (nowMs - entry.Value.LastUpdatedMs > maxAgeMs)
                {
                    _removeScratch.Add(entry.Key);
                }
            }

            for (int i = 0; i < _removeScratch.Count; i++)
            {
                map.Remove(_removeScratch[i]);
            }

            return _removeScratch.Count;
        }
    }
}
