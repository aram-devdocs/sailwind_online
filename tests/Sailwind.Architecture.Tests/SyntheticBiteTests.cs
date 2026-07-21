using System;
using System.Collections.Generic;
using Xunit;

namespace Sailwind.Architecture.Tests
{
    /// <summary>
    /// Proves every rule bites. Discovery on disk can pass vacuously (a rule finds no DLL
    /// for an assembly, so there is nothing to reject); these facts feed each validator a
    /// hand-built <see cref="AssemblyModel"/> that deliberately breaks exactly that rule
    /// and assert the validator reports it. A rule that could never fail would be no gate
    /// at all.
    /// </summary>
    public sealed class SyntheticBiteTests
    {
        private static readonly IReadOnlyList<string> NoTypes = Array.Empty<string>();

        private static AssemblyModel Model(string name, params string[] references) =>
            new AssemblyModel(name, references, NoTypes);

        [Fact]
        public void PureLibsAreGameFree_bites()
        {
            // A PURE library reaching into UnityEngine must be rejected.
            var models = new[]
            {
                Model(Rules.OnlineNet, Rules.Contracts, "UnityEngine.CoreModule"),
            };

            Assert.NotEmpty(Rules.PureLibsAreGameFree(models));
        }

        [Fact]
        public void ReferencesWithinAllowList_bites()
        {
            // Sailwind.Contracts is a leaf (allowed set is empty); any Sailwind.* reference
            // is out of bounds.
            var models = new[]
            {
                Model(Rules.Contracts, Rules.ApiAbstractions),
            };

            Assert.NotEmpty(Rules.ReferencesWithinAllowList(models));
        }

        [Fact]
        public void NoAppReferencesAnotherApp_bites()
        {
            var models = new[]
            {
                Model(Rules.Api, Rules.OnlineClient),
            };

            Assert.NotEmpty(Rules.NoAppReferencesAnotherApp(models));
        }

        [Fact]
        public void NoCycles_bites()
        {
            // Net <-> Sync mutual reference: a two-node cycle.
            var models = new[]
            {
                Model(Rules.OnlineNet, Rules.OnlineSync),
                Model(Rules.OnlineSync, Rules.OnlineNet),
            };

            Assert.NotEmpty(Rules.NoCycles(models));
        }

        [Fact]
        public void AppsAreThin_bitesOnMissingPackage()
        {
            // An app that composes no library assembly is not thin — it is empty.
            var models = new[]
            {
                new AssemblyModel(Rules.Api, new[] { "BepInEx" }, new[] { "Plugin" }),
            };

            Assert.NotEmpty(Rules.AppsAreThin(models));
        }

        [Fact]
        public void AppsAreThin_bitesOnLogicType()
        {
            // An app that declares a logic type (…Manager) is doing work a package should own.
            var models = new[]
            {
                new AssemblyModel(Rules.OnlineClient, new[] { Rules.OnlineNet }, new[] { "Plugin", "SessionManager" }),
            };

            Assert.NotEmpty(Rules.AppsAreThin(models));
        }
    }
}
