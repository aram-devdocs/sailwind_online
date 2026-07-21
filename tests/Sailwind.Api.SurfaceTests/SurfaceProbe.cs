using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using Mono.Cecil;
using Xunit;

namespace Sailwind.Api.SurfaceTests
{
    /// <summary>
    /// Shared Cecil-backed verifier used by the generated SurfaceContract facts and
    /// the smoke test. When lib/ is absent the Require* helpers no-op so the project
    /// stays valid in environments without the game DLL; the real assertions run
    /// wherever <see cref="LibAvailable"/> is true (local / self-hosted).
    /// </summary>
    public static class SurfaceProbe
    {
        private static readonly Lazy<string> AssemblyPath = new(FindGameAssembly);
        private static readonly Lazy<ModuleDefinition> Module = new(LoadModule);

        public static bool LibAvailable => AssemblyPath.Value != null && File.Exists(AssemblyPath.Value);

        public static void RequireType(string simpleName)
        {
            if (!LibAvailable) return;
            Assert.True(FindType(simpleName) != null, $"type '{simpleName}' not found in the game assembly");
        }

        public static void RequireMember(string type, string member, string kind, bool isStatic)
        {
            if (!LibAvailable) return;
            var t = FindType(type);
            Assert.True(t != null, $"type '{type}' not found in the game assembly");

            bool found;
            switch (kind)
            {
                case "field":
                    found = t.Fields.Any(f => f.Name == member && f.IsStatic == isStatic);
                    break;
                case "property":
                    found = t.Properties.Any(p => p.Name == member &&
                        ((p.GetMethod ?? p.SetMethod)?.IsStatic ?? false) == isStatic);
                    break;
                case "method":
                    found = t.Methods.Any(m => m.Name == member && m.IsStatic == isStatic);
                    break;
                default:
                    found = false;
                    break;
            }

            Assert.True(found, $"{type}.{member} ({kind}, static={isStatic}) not found in the game assembly");
        }

        private static TypeDefinition FindType(string simpleName)
        {
            foreach (var t in AllTypes(Module.Value.Types))
                if (t.Name == simpleName)
                    return t;
            return null;
        }

        private static IEnumerable<TypeDefinition> AllTypes(IEnumerable<TypeDefinition> types)
        {
            foreach (var t in types)
            {
                yield return t;
                if (t.HasNestedTypes)
                    foreach (var n in AllTypes(t.NestedTypes))
                        yield return n;
            }
        }

        private static ModuleDefinition LoadModule() =>
            LibAvailable ? ModuleDefinition.ReadModule(AssemblyPath.Value) : null;

        private static string FindGameAssembly()
        {
            var dir = new DirectoryInfo(AppContext.BaseDirectory);
            while (dir != null)
            {
                if (File.Exists(Path.Combine(dir.FullName, "global.json")))
                    return Path.Combine(dir.FullName, "lib", "Assembly-CSharp.dll");
                dir = dir.Parent;
            }
            return null;
        }
    }
}
