namespace Sailwind.Api
{
    /// <summary>Player boat reader — an adapter over the local player's Boat.</summary>
    public interface IPlayerBoatReader
    {
        bool HasBoat { get; }
        BoatPose Pose { get; }
    }
}
