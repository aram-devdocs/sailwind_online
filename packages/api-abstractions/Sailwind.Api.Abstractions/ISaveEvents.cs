using System;

namespace Sailwind.Api
{
    /// <summary>
    /// Save lifecycle state and events exposed from SaveLoadManager.
    /// </summary>
    public interface ISaveEvents
    {
        /// <summary>
        /// True after the game reaches the common world-ready point used by both
        /// new-game and continue flows. A failed game-member read returns false.
        /// </summary>
        bool IsWorldReady { get; }

        event Action WorldLoaded;
        event Action SaveCompleted;
    }
}
