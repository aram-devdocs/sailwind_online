using System;
using Sailwind.Api;
using Xunit;

namespace Sailwind.Api.SurfaceManifest.Tests
{
    public sealed class WorldReadyPollerTests
    {
        [Fact]
        public void Poll_SignalsOnce_OnlyAfterWorldReadyMarkerIsTrue()
        {
            var poller = new WorldReadyPoller();
            var ready = false;
            var signals = 0;

            poller.Poll(() => ready, () => signals++);
            ready = true;
            poller.Poll(() => ready, () => signals++);
            poller.Poll(() => ready, () => signals++);

            Assert.Equal(1, signals);
        }

        [Fact]
        public void Poll_WhenMarkerReadThrows_DegradesAndCanRetry()
        {
            var poller = new WorldReadyPoller();
            var signals = 0;

            Action poll = () =>
                poller.Poll(() => throw new InvalidOperationException("read failed"), () => signals++);
            var exception = Record.Exception(poll);
            poller.Poll(() => true, () => signals++);

            Assert.Null(exception);
            Assert.Equal(1, signals);
        }
    }
}
