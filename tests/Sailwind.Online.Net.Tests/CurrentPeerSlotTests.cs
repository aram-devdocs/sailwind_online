using Sailwind.Online.Client.Net;
using Xunit;

namespace Sailwind.Online.Net.Tests
{
    public sealed class CurrentPeerSlotTests
    {
        [Fact]
        public void LocalDropThenReplacement_RejectsOldCallbacksButAllowsCurrentDisconnect()
        {
            var slot = new CurrentPeerSlot<object>();
            var oldPeer = new object();
            var currentPeer = new object();

            slot.Set(oldPeer);
            Assert.Same(oldPeer, slot.Clear());

            slot.Set(currentPeer);
            Assert.False(slot.IsCurrent(oldPeer));
            Assert.False(slot.TryClear(oldPeer));
            Assert.True(slot.IsCurrent(currentPeer));
            Assert.Same(currentPeer, slot.Value);

            Assert.True(slot.TryClear(currentPeer));
            Assert.Null(slot.Value);
        }
    }
}
