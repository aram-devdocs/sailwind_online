using Sailwind.Online.Client.Net;
using Xunit;

namespace Sailwind.Online.Net.Tests
{
    public sealed class CurrentPeerSlotTests
    {
        [Fact]
        public void PendingPeer_NullReplacementAttempt_PreservesCurrentPeer()
        {
            var slot = new CurrentPeerSlot<object>();
            var pendingPeer = new object();

            slot.SetIfPresent(pendingPeer);
            slot.SetIfPresent(null);

            Assert.Same(pendingPeer, slot.Value);
            Assert.True(slot.IsCurrent(pendingPeer));
        }

        [Fact]
        public void LocalDropThenReplacement_RejectsOldCallbacksButAllowsCurrentDisconnect()
        {
            var slot = new CurrentPeerSlot<object>();
            var oldPeer = new object();
            var currentPeer = new object();

            slot.SetIfPresent(oldPeer);
            Assert.Same(oldPeer, slot.Clear());

            slot.SetIfPresent(currentPeer);
            Assert.False(slot.IsCurrent(oldPeer));
            Assert.False(slot.TryClear(oldPeer));
            Assert.True(slot.IsCurrent(currentPeer));
            Assert.Same(currentPeer, slot.Value);

            Assert.True(slot.TryClear(currentPeer));
            Assert.Null(slot.Value);
        }
    }
}
