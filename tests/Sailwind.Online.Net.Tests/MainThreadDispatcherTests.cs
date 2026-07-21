using System;
using System.Collections.Generic;
using Sailwind.Online.Client.Runtime;
using Xunit;

namespace Sailwind.Online.Net.Tests
{
    /// <summary>
    /// The main-thread work queue: FIFO draining, null rejection, pending-count accuracy,
    /// and the guarantee that one throwing action neither stops the drain nor escapes the
    /// caller (it is surfaced through the error callback instead).
    /// </summary>
    public sealed class MainThreadDispatcherTests
    {
        [Fact]
        public void Drain_RunsQueuedActionsInFifoOrder()
        {
            var dispatcher = new MainThreadDispatcher();
            var order = new List<int>();
            dispatcher.Enqueue(() => order.Add(1));
            dispatcher.Enqueue(() => order.Add(2));
            dispatcher.Enqueue(() => order.Add(3));

            dispatcher.Drain(null);

            Assert.Equal(new[] { 1, 2, 3 }, order);
            Assert.Equal(0, dispatcher.PendingCount);
        }

        [Fact]
        public void Enqueue_Null_IsIgnored()
        {
            var dispatcher = new MainThreadDispatcher();
            dispatcher.Enqueue(null);
            Assert.Equal(0, dispatcher.PendingCount);
        }

        [Fact]
        public void PendingCount_ReflectsUndrainedWork()
        {
            var dispatcher = new MainThreadDispatcher();
            dispatcher.Enqueue(() => { });
            dispatcher.Enqueue(() => { });
            Assert.Equal(2, dispatcher.PendingCount);
        }

        [Fact]
        public void Drain_ThrowingAction_IsReportedAndDoesNotStopDrain()
        {
            var dispatcher = new MainThreadDispatcher();
            var ran = new List<int>();
            var errors = new List<Exception>();

            dispatcher.Enqueue(() => ran.Add(1));
            dispatcher.Enqueue(() => throw new InvalidOperationException("boom"));
            dispatcher.Enqueue(() => ran.Add(3));

            dispatcher.Drain(errors.Add);

            Assert.Equal(new[] { 1, 3 }, ran);
            Assert.Single(errors);
            Assert.IsType<InvalidOperationException>(errors[0]);
            Assert.Equal(0, dispatcher.PendingCount);
        }

        [Fact]
        public void Drain_ThrowingAction_WithoutErrorCallback_DoesNotEscape()
        {
            var dispatcher = new MainThreadDispatcher();
            dispatcher.Enqueue(() => throw new InvalidOperationException("boom"));

            dispatcher.Drain(null); // must not throw
            Assert.Equal(0, dispatcher.PendingCount);
        }
    }
}
