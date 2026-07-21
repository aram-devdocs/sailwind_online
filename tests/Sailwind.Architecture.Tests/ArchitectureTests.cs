using System.Collections.Generic;
using Xunit;

namespace Sailwind.Architecture.Tests
{
    /// <summary>
    /// The real graph. Each fact scans the built DLLs and asserts the corresponding rule
    /// finds no violation. When the game-coupled DLLs are absent (CI game-free tier), the
    /// rule simply sees fewer assemblies and still holds — see the paired
    /// <see cref="SyntheticBiteTests"/> for the proof that every rule actually bites.
    /// </summary>
    public sealed class ArchitectureTests
    {
        private static readonly IReadOnlyList<AssemblyModel> Assemblies = AssemblyScanner.Scan();

        [Fact]
        public void PureLibsAreGameFree()
        {
            Assert.Empty(Rules.PureLibsAreGameFree(Assemblies));
        }

        [Fact]
        public void ReferencesWithinAllowList()
        {
            Assert.Empty(Rules.ReferencesWithinAllowList(Assemblies));
        }

        [Fact]
        public void NoAppReferencesAnotherApp()
        {
            Assert.Empty(Rules.NoAppReferencesAnotherApp(Assemblies));
        }

        [Fact]
        public void NoCycles()
        {
            Assert.Empty(Rules.NoCycles(Assemblies));
        }

        [Fact]
        public void AppsAreThin()
        {
            Assert.Empty(Rules.AppsAreThin(Assemblies));
        }

        [Fact]
        public void ScanFindsTheGameFreeCore()
        {
            // Guards against the whole suite passing only because discovery silently found
            // nothing: the four pure libraries build in every tier, so they MUST be present.
            var names = new HashSet<string>();
            foreach (var m in Assemblies)
            {
                names.Add(m.Name);
            }

            Assert.Contains(Rules.Contracts, names);
            Assert.Contains(Rules.ApiAbstractions, names);
            Assert.Contains(Rules.OnlineNet, names);
            Assert.Contains(Rules.OnlineSync, names);
        }
    }
}
