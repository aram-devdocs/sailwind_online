using UnityEngine;

namespace Sailwind.Api
{
    /// <summary>Ambient wind reader — an adapter over the game's Wind.</summary>
    public interface IWindReader
    {
        Vector3 Ambient { get; }
    }
}
