namespace Sailwind.Online.Client.Net
{
    /// <summary>
    /// Logging seam for the transport. Keeping it here (rather than taking a BepInEx
    /// <c>ManualLogSource</c> directly) is what makes this package game-free: the app
    /// supplies a BepInEx-backed adapter, and unit tests supply a no-op or capturing one.
    /// </summary>
    public interface INetLog
    {
        void LogDebug(string message);
        void LogInfo(string message);
        void LogWarning(string message);
        void LogError(string message);
    }
}
