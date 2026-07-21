using System.Numerics;

namespace Sailwind.Api
{
    /// <summary>Ambient wind reader — an adapter over the game's Wind. The vector uses
    /// <see cref="System.Numerics"/> so the contract stays UnityEngine-free.</summary>
    public interface IWindReader
    {
        Vector3 Ambient { get; }
    }
}
