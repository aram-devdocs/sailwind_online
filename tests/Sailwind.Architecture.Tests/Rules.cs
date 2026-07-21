using System;
using System.Collections.Generic;
using System.Linq;

namespace Sailwind.Architecture.Tests
{
    /// <summary>
    /// The layered architecture of the monorepo, encoded as data. Every validator is a
    /// pure function over a set of <see cref="AssemblyModel"/> that returns the list of
    /// violations it found (empty == the rule holds). Because the functions never touch
    /// disk, the real graph and the synthetic bite-fixtures both flow through the same
    /// code.
    /// </summary>
    public static class Rules
    {
        // ---- Nodes of the dependency DAG (the layered architecture assemblies) --------
        public const string Contracts = "Sailwind.Contracts";
        public const string ApiAbstractions = "Sailwind.Api.Abstractions";
        public const string OnlineNet = "Sailwind.Online.Net";
        public const string OnlineSync = "Sailwind.Online.Sync";
        public const string ApiAdapters = "Sailwind.Api.Adapters";
        public const string Api = "Sailwind.API";
        public const string OnlineClient = "Sailwind.Online.Client";

        /// <summary>Pure (game-free) libraries: they may never reach the game assembly.</summary>
        public static readonly IReadOnlyCollection<string> PureAssemblies = new HashSet<string>
        {
            Contracts, ApiAbstractions, OnlineNet, OnlineSync,
        };

        /// <summary>The two thin composition-root plugins.</summary>
        public static readonly IReadOnlyCollection<string> AppAssemblies = new HashSet<string>
        {
            Api, OnlineClient,
        };

        /// <summary>Assemblies permitted to reference the game (Unity / Assembly-CSharp / BepInEx / Harmony / Crest).</summary>
        public static readonly IReadOnlyCollection<string> GameCoupledAssemblies = new HashSet<string>
        {
            ApiAdapters, Api, OnlineClient,
        };

        /// <summary>
        /// The library assemblies an app composes. A thin app MUST reference at least one
        /// of these, because a composition root that pulls in no library is not composing
        /// anything.
        /// </summary>
        public static readonly IReadOnlyCollection<string> PackageAssemblies = new HashSet<string>
        {
            Contracts, ApiAbstractions, OnlineNet, OnlineSync, ApiAdapters,
        };

        /// <summary>
        /// The only intra-repo (Sailwind.*) reference edges the DAG permits. A key's value
        /// is the set of Sailwind.* assemblies it is allowed to reference.
        ///
        /// NOTE: Sailwind.Online.Net's allowed set is widened from the drafted
        /// { Sailwind.Contracts } to also include Sailwind.Api.Abstractions and
        /// Sailwind.Online.Sync, because the real build emits those references — the
        /// transport reports poses through BoatPose (Sailwind.Api.Abstractions) and feeds
        /// the snapshot cache in Sailwind.Online.Sync. The edge still points strictly down
        /// the layers (no cycle), so the architecture is intact.
        /// </summary>
        public static readonly IReadOnlyDictionary<string, IReadOnlyCollection<string>> AllowedReferences =
            new Dictionary<string, IReadOnlyCollection<string>>
            {
                [Contracts] = new HashSet<string>(),
                [ApiAbstractions] = new HashSet<string>(),
                [OnlineNet] = new HashSet<string> { Contracts, ApiAbstractions, OnlineSync },
                [OnlineSync] = new HashSet<string> { ApiAbstractions },
                [ApiAdapters] = new HashSet<string> { ApiAbstractions, Contracts },
                [Api] = new HashSet<string> { ApiAdapters, ApiAbstractions, Contracts },
                [OnlineClient] = new HashSet<string> { OnlineNet, OnlineSync, ApiAbstractions, Contracts },
            };

        /// <summary>The architecture assemblies the DAG rules reason about.</summary>
        public static readonly IReadOnlyCollection<string> KnownAssemblies =
            new HashSet<string>(AllowedReferences.Keys);

        /// <summary>Reference-name prefixes that mean "this touches the game engine".</summary>
        public static readonly IReadOnlyList<string> GamePrefixes = new[]
        {
            "UnityEngine", "Assembly-CSharp", "Crest", "BepInEx", "0Harmony",
        };

        /// <summary>Type-name suffixes that denote logic — forbidden inside a thin app.</summary>
        public static readonly IReadOnlyList<string> LogicSuffixes = new[]
        {
            "Adapter", "Codec", "Cache", "Reporter", "Service", "Manager",
        };

        /// <summary>Type-name suffixes a thin app IS allowed to declare (wiring, not logic).</summary>
        public static readonly IReadOnlyList<string> AllowedAppSuffixes = new[]
        {
            "Plugin", "Hud", "NetLog", "Dispatcher",
        };

        private static bool IsSailwindRef(string reference) =>
            reference.StartsWith("Sailwind.", StringComparison.Ordinal);

        private static bool IsGameRef(string reference) =>
            GamePrefixes.Any(p => reference.StartsWith(p, StringComparison.Ordinal));

        // ---- Rule 1 -------------------------------------------------------------------
        /// <summary>
        /// PURE libraries reference nothing from the game engine (Unity / Assembly-CSharp /
        /// Crest / BepInEx / Harmony).
        /// </summary>
        public static IReadOnlyList<string> PureLibsAreGameFree(IEnumerable<AssemblyModel> models)
        {
            var violations = new List<string>();
            foreach (var m in models.Where(m => PureAssemblies.Contains(m.Name)))
            {
                foreach (var reference in m.References.Where(IsGameRef))
                {
                    violations.Add($"{m.Name} is a PURE library but references game assembly '{reference}'.");
                }
            }

            return violations;
        }

        // ---- Rule 2 -------------------------------------------------------------------
        /// <summary>
        /// Every Sailwind.* reference of a known assembly is in that assembly's allowed set.
        /// </summary>
        public static IReadOnlyList<string> ReferencesWithinAllowList(IEnumerable<AssemblyModel> models)
        {
            var violations = new List<string>();
            foreach (var m in models.Where(m => AllowedReferences.ContainsKey(m.Name)))
            {
                var allowed = AllowedReferences[m.Name];
                foreach (var reference in m.References.Where(IsSailwindRef))
                {
                    if (reference == m.Name)
                    {
                        continue;
                    }

                    if (!allowed.Contains(reference))
                    {
                        violations.Add($"{m.Name} references '{reference}', which is outside its allowed set {{ {string.Join(", ", allowed.OrderBy(x => x))} }}.");
                    }
                }
            }

            return violations;
        }

        // ---- Rule 3 -------------------------------------------------------------------
        /// <summary>Neither app references the other app.</summary>
        public static IReadOnlyList<string> NoAppReferencesAnotherApp(IEnumerable<AssemblyModel> models)
        {
            var violations = new List<string>();
            foreach (var m in models.Where(m => AppAssemblies.Contains(m.Name)))
            {
                foreach (var reference in m.References)
                {
                    if (reference != m.Name && AppAssemblies.Contains(reference))
                    {
                        violations.Add($"app {m.Name} references the other app '{reference}'.");
                    }
                }
            }

            return violations;
        }

        // ---- Rule 4 -------------------------------------------------------------------
        /// <summary>
        /// The Sailwind.* reference graph (restricted to present, known assemblies) is
        /// acyclic. Runs Kahn's topological sort; any residual node sits on a cycle.
        /// </summary>
        public static IReadOnlyList<string> NoCycles(IEnumerable<AssemblyModel> models)
        {
            var present = models
                .Where(m => KnownAssemblies.Contains(m.Name))
                .GroupBy(m => m.Name)
                .ToDictionary(g => g.Key, g => g.First());

            var nodes = new HashSet<string>(present.Keys);

            // Adjacency: n -> successors, restricted to Sailwind.* refs that are present nodes.
            var successors = nodes.ToDictionary(
                n => n,
                n => present[n].References.Where(r => r != n && nodes.Contains(r)).Distinct().ToList());

            var inDegree = nodes.ToDictionary(n => n, _ => 0);
            foreach (var n in nodes)
            {
                foreach (var s in successors[n])
                {
                    inDegree[s]++;
                }
            }

            var queue = new Queue<string>(nodes.Where(n => inDegree[n] == 0));
            var removed = 0;
            while (queue.Count > 0)
            {
                var n = queue.Dequeue();
                removed++;
                foreach (var s in successors[n])
                {
                    if (--inDegree[s] == 0)
                    {
                        queue.Enqueue(s);
                    }
                }
            }

            if (removed == nodes.Count)
            {
                return Array.Empty<string>();
            }

            var residual = nodes.Where(n => inDegree[n] > 0).OrderBy(x => x);
            return new[] { $"reference cycle among: {string.Join(", ", residual)}." };
        }

        // ---- Rule 5 -------------------------------------------------------------------
        /// <summary>
        /// Each app is thin: (a) it references at least one package (library) assembly, and
        /// (b) it declares no type whose name ends in a logic suffix. Wiring types
        /// (Plugin / *Hud / *NetLog / *Dispatcher) are fine.
        /// </summary>
        public static IReadOnlyList<string> AppsAreThin(IEnumerable<AssemblyModel> models)
        {
            var violations = new List<string>();
            foreach (var m in models.Where(m => AppAssemblies.Contains(m.Name)))
            {
                if (!m.References.Any(r => PackageAssemblies.Contains(r)))
                {
                    violations.Add($"app {m.Name} references no package assembly; a thin app must compose at least one library.");
                }

                foreach (var typeName in m.DeclaredTypeNames)
                {
                    var suffix = LogicSuffixes.FirstOrDefault(s => typeName.EndsWith(s, StringComparison.Ordinal));
                    if (suffix != null)
                    {
                        violations.Add($"app {m.Name} declares logic type '{typeName}' (ends in '{suffix}'); move it into a package.");
                    }
                }
            }

            return violations;
        }
    }
}
