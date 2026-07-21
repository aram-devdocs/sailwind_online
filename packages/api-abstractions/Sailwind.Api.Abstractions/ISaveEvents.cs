using System;

namespace Sailwind.Api
{
    /// <summary>
    /// Save lifecycle events — raised from Harmony postfixes on SaveLoadManager.
    /// </summary>
    public interface ISaveEvents
    {
        event Action WorldLoaded;
        event Action SaveCompleted;
    }
}
