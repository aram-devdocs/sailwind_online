using BepInEx.Logging;
using Sailwind.Online.Client.Net;

namespace Sailwind.Online.Client
{
    /// <summary>
    /// BepInEx-backed <see cref="INetLog"/>: the game-coupled glue that lets the pure
    /// Sailwind.Online.Net package log through the plugin's <see cref="ManualLogSource"/>
    /// without taking a BepInEx dependency itself.
    /// </summary>
    internal sealed class BepInExNetLog : INetLog
    {
        private readonly ManualLogSource _log;

        public BepInExNetLog(ManualLogSource log)
        {
            _log = log;
        }

        public void LogDebug(string message) => _log.LogDebug(message);
        public void LogInfo(string message) => _log.LogInfo(message);
        public void LogWarning(string message) => _log.LogWarning(message);
        public void LogError(string message) => _log.LogError(message);
    }
}
