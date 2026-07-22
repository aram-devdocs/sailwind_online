using System;
using Sailwind.Api;
using Xunit;

namespace Sailwind.Api.SurfaceManifest.Tests
{
    public sealed class WorldReadyPollerTests
    {
        [Fact]
        public void SignalReady_WhenSubscriberThrows_InvokesLaterSubscribersOnce()
        {
            var expected = new InvalidOperationException("subscriber failed");
            var laterSignals = 0;
            Action throwingSubscriber = () => throw expected;
            Action laterSubscriber = () => laterSignals++;
            SailwindApi.Ready += throwingSubscriber;
            SailwindApi.Ready += laterSubscriber;

            try
            {
                var actual = Assert.Throws<InvalidOperationException>(SailwindApi.SignalReady);
                SailwindApi.SignalReady();

                Assert.Same(expected, actual);
                Assert.Equal(1, laterSignals);
            }
            finally
            {
                SailwindApi.Ready -= throwingSubscriber;
                SailwindApi.Ready -= laterSubscriber;
            }
        }

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
