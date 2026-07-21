namespace Sailwind.Api
{
    /// <summary>World clock reader — an adapter over the game's Sun / GameState.</summary>
    public interface IGameClock
    {
        int Day { get; }
        float TimeOfDay { get; }
        float MoonPhase { get; }
    }
}
