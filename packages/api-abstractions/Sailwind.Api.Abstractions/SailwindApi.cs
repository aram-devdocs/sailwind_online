using System;
using System.Runtime.ExceptionServices;

namespace Sailwind.Api
{
    /// <summary>
    /// The public entry point other mods depend on. Populated by the Sailwind.API
    /// plugin at startup; the reader services are non-null once <see cref="Ready"/>
    /// has fired (world loaded and surface verified), and null before then.
    /// </summary>
    public static class SailwindApi
    {
        /// <summary>Version of the Sailwind.API plugin; set by the host at startup.</summary>
        public static string? Version { get; internal set; }

        /// <summary>Hash of the verified game surface; set by the host at startup.</summary>
        public static string? SurfaceHash { get; internal set; }

        public static ApiCompat? Compat { get; internal set; }

        public static IGameClock? Clock { get; internal set; }
        public static IWindReader? Wind { get; internal set; }
        public static IPlayerBoatReader? PlayerBoat { get; internal set; }
        public static ISaveEvents? SaveEvents { get; internal set; }

        private static readonly object Gate = new object();
        private static Action? _ready;
        private static bool _isReady;

        /// <summary>Fires once the world is loaded and the surface has been verified.
        /// Handlers added after the fact are invoked immediately.</summary>
        public static event Action Ready
        {
            add
            {
                bool fireNow;
                lock (Gate)
                {
                    _ready += value;
                    fireNow = _isReady;
                }
                if (fireNow) value?.Invoke();
            }
            remove
            {
                lock (Gate)
                {
                    _ready -= value;
                }
            }
        }

        internal static void SignalReady()
        {
            Action? handlers;
            lock (Gate)
            {
                if (_isReady) return;
                _isReady = true;
                handlers = _ready;
            }

            Exception? firstFailure = null;
            if (handlers != null)
            {
                foreach (Action handler in handlers.GetInvocationList())
                {
                    try
                    {
                        handler();
                    }
                    catch (Exception exception)
                    {
                        if (firstFailure == null)
                        {
                            firstFailure = exception;
                        }
                    }
                }
            }

            if (firstFailure != null)
            {
                ExceptionDispatchInfo.Capture(firstFailure).Throw();
            }
        }
    }
}
