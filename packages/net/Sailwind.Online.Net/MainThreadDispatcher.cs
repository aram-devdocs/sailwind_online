using System;
using System.Collections.Concurrent;

namespace Sailwind.Online.Client.Runtime
{
    /// <summary>
    /// A thin, thread-safe work queue drained on the Unity main thread.
    /// At init-0 the LiteNetLib manager is polled synchronously from Update(), so all
    /// network handlers already run on the main thread and nothing needs marshalling yet.
    /// This queue exists so future off-thread work (disk I/O, background decode) can hand
    /// results back to the main thread without introducing a scheduler later.
    /// </summary>
    public sealed class MainThreadDispatcher
    {
        private readonly ConcurrentQueue<Action> _queue = new ConcurrentQueue<Action>();

        /// <summary>Queue work to run on the next main-thread <see cref="Drain"/>. Null is ignored.</summary>
        public void Enqueue(Action? work)
        {
            if (work != null)
            {
                _queue.Enqueue(work);
            }
        }

        /// <summary>Number of actions currently waiting to run.</summary>
        public int PendingCount
        {
            get { return _queue.Count; }
        }

        /// <summary>
        /// Run every queued action on the calling (main) thread. An exception in one action
        /// is surfaced to <paramref name="onError"/> (if provided) and does not stop the drain.
        /// </summary>
        public void Drain(Action<Exception>? onError)
        {
            while (_queue.TryDequeue(out Action work))
            {
                try
                {
                    work();
                }
                catch (Exception ex)
                {
                    if (onError != null)
                    {
                        onError(ex);
                    }
                }
            }
        }
    }
}
