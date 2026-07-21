using Xunit;

namespace Sailwind.Api.SurfaceTests
{
    /// <summary>
    /// Hand-written sanity check that the project + Cecil pipeline are wired up. The
    /// real contract lives in the generated SurfaceContract.g.cs (one fact per
    /// manifest entry). This fact no-ops when lib/ is absent so the project is valid
    /// without the game DLL, and does a real load when it is present.
    /// </summary>
    public sealed class SmokeTest
    {
        [Fact]
        public void GameAssembly_LoadsAndHasTypes_WhenLibPresent()
        {
            if (!SurfaceProbe.LibAvailable)
                return; // deferred: needs lib/Assembly-CSharp.dll (run locally after `make setup`)

            // A recon-confirmed type must resolve, proving the Cecil reader works.
            SurfaceProbe.RequireType("GameState");
        }
    }
}
