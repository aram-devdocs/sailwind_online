using Sailwind.Online.Client.Sync;
using Xunit;

namespace Sailwind.Online.Sync.Tests
{
    /// <summary>
    /// The remote-entity cache: upsert/latest tracking, the fixed-capacity timestamped
    /// ring buffer that snapshot interpolation samples from (chronological ordering and
    /// eviction), monotonic server-tick tracking, staleness pruning, and reset.
    /// </summary>
    public sealed class SnapshotCacheTests
    {
        [Fact]
        public void EntityTrack_RingBuffer_RetainsNewestInChronologicalOrder()
        {
            var track = new EntityTrack(id: 1, capacity: 3);
            for (uint i = 1; i <= 4; i++)
            {
                track.Push(new EntitySample { TMs = i, ReceivedMs = i });
            }

            // Capacity 3, four pushes: the first sample is evicted.
            Assert.Equal(3, track.SampleCount);
            Assert.Equal(4u, track.Latest.TMs);
            Assert.Equal(4L, track.LastUpdatedMs);

            // Index 0 = oldest retained (the 2nd push) .. 2 = newest (the 4th push).
            Assert.True(track.TryGetSample(0, out var oldest));
            Assert.Equal(2u, oldest.TMs);
            Assert.True(track.TryGetSample(2, out var newest));
            Assert.Equal(4u, newest.TMs);

            Assert.False(track.TryGetSample(3, out _));
            Assert.False(track.TryGetSample(-1, out _));
        }

        [Fact]
        public void UpsertPlayer_TracksLatestSampleAndCount()
        {
            var cache = new SnapshotCache();
            cache.UpsertPlayer(7, new EntitySample
            {
                Pos = new NetVec3(1f, 2f, 3f),
                TMs = 100,
                ReceivedMs = 1000,
            });

            Assert.Equal(1, cache.PlayerCount);
            Assert.Equal(0, cache.BoatCount);
            Assert.True(cache.TryGetPlayer(7, out var track));
            Assert.NotNull(track);
            Assert.Equal(100u, track.Latest.TMs);
            Assert.Equal(3f, track.Latest.Pos.Z);
        }

        [Fact]
        public void UpsertPlayer_SameId_UpdatesInPlace()
        {
            var cache = new SnapshotCache();
            cache.UpsertPlayer(7, new EntitySample { TMs = 1, ReceivedMs = 10 });
            cache.UpsertPlayer(7, new EntitySample { TMs = 2, ReceivedMs = 20 });

            Assert.Equal(1, cache.PlayerCount);
            Assert.True(cache.TryGetPlayer(7, out var track));
            Assert.Equal(2u, track.Latest.TMs);
            Assert.Equal(2, track.SampleCount);
        }

        [Fact]
        public void ObserveServerTick_AdvancesMonotonically()
        {
            var cache = new SnapshotCache();
            cache.ObserveServerTick(5);
            Assert.Equal(5u, cache.LastServerTick);

            cache.ObserveServerTick(3); // out-of-order tick ignored
            Assert.Equal(5u, cache.LastServerTick);

            cache.ObserveServerTick(9);
            Assert.Equal(9u, cache.LastServerTick);
        }

        [Fact]
        public void PruneStale_RemovesOnlyEntriesOlderThanMaxAge()
        {
            var cache = new SnapshotCache();
            cache.UpsertPlayer(1, new EntitySample { ReceivedMs = 1000 });
            cache.UpsertBoat(2, new EntitySample { ReceivedMs = 5000 });

            // now=6500, maxAge=2000: player (age 5500) is stale, boat (age 1500) is fresh.
            int removed = cache.PruneStale(nowMs: 6500, maxAgeMs: 2000);

            Assert.Equal(1, removed);
            Assert.Equal(0, cache.PlayerCount);
            Assert.Equal(1, cache.BoatCount);
        }

        [Fact]
        public void Clear_ResetsCountsAndServerTick()
        {
            var cache = new SnapshotCache();
            cache.UpsertPlayer(1, new EntitySample { ReceivedMs = 1 });
            cache.UpsertBoat(2, new EntitySample { ReceivedMs = 1 });
            cache.ObserveServerTick(42);

            cache.Clear();

            Assert.Equal(0, cache.PlayerCount);
            Assert.Equal(0, cache.BoatCount);
            Assert.Equal(0u, cache.LastServerTick);
        }
    }
}
