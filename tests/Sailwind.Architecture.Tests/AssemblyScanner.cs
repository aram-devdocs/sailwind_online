using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using Mono.Cecil;

namespace Sailwind.Architecture.Tests
{
    /// <summary>
    /// Discovers the built architecture DLLs and reads them as <see cref="AssemblyModel"/>
    /// through Cecil metadata only (never Assembly.Load, so no game/Unity dependency is
    /// ever resolved). Discovery walks the repo for <c>**/bin/Release/**/Sailwind.*.dll</c>,
    /// keeps only each project's own output (path contains <c>/&lt;AssemblyName&gt;/bin/</c>,
    /// which drops the copies plugins fan out into their plugin folders), and skips test
    /// assemblies. Unreadable or locked DLLs are swallowed — the corresponding rule then
    /// passes vacuously, exactly why every rule is also proven against a synthetic fixture.
    /// </summary>
    public static class AssemblyScanner
    {
        public static IReadOnlyList<AssemblyModel> Scan()
        {
            var repoRoot = FindRepoRoot();
            if (repoRoot == null)
            {
                return Array.Empty<AssemblyModel>();
            }

            var models = new List<AssemblyModel>();
            var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);

            foreach (var dll in Directory.EnumerateFiles(repoRoot, "Sailwind.*.dll", SearchOption.AllDirectories))
            {
                var norm = dll.Replace('\\', '/');

                if (norm.IndexOf("/bin/Release/", StringComparison.OrdinalIgnoreCase) < 0)
                {
                    continue;
                }

                var name = Path.GetFileNameWithoutExtension(dll);

                // Owning-project output only: keeps packages/net/Sailwind.Online.Net/bin/...
                // but drops the byte-identical copies plugins ship in their bin folders.
                if (norm.IndexOf("/" + name + "/bin/", StringComparison.OrdinalIgnoreCase) < 0)
                {
                    continue;
                }

                // Skip test assemblies (Sailwind.*.Tests, Sailwind.Api.SurfaceTests, this project).
                if (name.EndsWith("Tests", StringComparison.OrdinalIgnoreCase))
                {
                    continue;
                }

                if (!seen.Add(name))
                {
                    continue;
                }

                try
                {
                    // InMemory copies the image and closes the file handle, so scanning never
                    // locks a DLL another build step might rewrite.
                    using var module = ModuleDefinition.ReadModule(dll, new ReaderParameters { InMemory = true });
                    var references = module.AssemblyReferences.Select(r => r.Name).ToList();
                    var typeNames = CollectTypeNames(module.Types).ToList();
                    models.Add(new AssemblyModel(name, references, typeNames));
                }
                catch
                {
                    // Unreadable / locked / not a managed image: skip it.
                }
            }

            return models;
        }

        private static IEnumerable<string> CollectTypeNames(IEnumerable<TypeDefinition> types)
        {
            foreach (var t in types)
            {
                // Skip the <Module> pseudo-type and compiler-generated closures/anon types.
                if (!t.Name.StartsWith("<", StringComparison.Ordinal) && t.Name != "<Module>")
                {
                    yield return t.Name;
                }

                if (t.HasNestedTypes)
                {
                    foreach (var n in CollectTypeNames(t.NestedTypes))
                    {
                        yield return n;
                    }
                }
            }
        }

        private static string FindRepoRoot()
        {
            var dir = new DirectoryInfo(AppContext.BaseDirectory);
            while (dir != null)
            {
                if (File.Exists(Path.Combine(dir.FullName, "global.json")))
                {
                    return dir.FullName;
                }

                dir = dir.Parent;
            }

            return null;
        }
    }
}
