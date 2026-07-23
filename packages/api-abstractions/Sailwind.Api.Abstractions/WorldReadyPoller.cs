using System;

namespace Sailwind.Api
{
    /// <summary>
    /// Converts a polled world-ready marker into a single API-ready signal without
    /// allowing a failed game read or subscriber to escape into Unity's update loop.
    /// </summary>
    internal sealed class WorldReadyPoller
    {
        private bool _signaled;

        public void Poll(Func<bool> isWorldReady, Action signalReady)
        {
            if (_signaled || isWorldReady == null || signalReady == null)
            {
                return;
            }

            bool ready;
            try
            {
                ready = isWorldReady();
            }
            catch
            {
                return;
            }

            if (!ready)
            {
                return;
            }

            _signaled = true;
            try
            {
                signalReady();
            }
            catch
            {
                // A Ready subscriber is mod code and must not break the game loop.
            }
        }
    }
}
