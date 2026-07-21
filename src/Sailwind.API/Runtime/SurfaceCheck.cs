using System;
using System.Collections.Generic;
using System.Reflection;
using Sailwind.Api.Generated;

namespace Sailwind.Api.Runtime
{
    /// <summary>
    /// Verifies the embedded surface manifest against the loaded game assembly at
    /// plugin startup using plain reflection (privates included). Collects all drift
    /// and returns a single report; the caller degrades affected services rather
    /// than crashing.
    /// </summary>
    internal static class SurfaceCheck
    {
        public static ApiCompat Run()
        {
            var members = SurfaceManifest.Members;
            var missing = new List<string>();

            var index = BuildTypeIndex(GameBind.GameAssembly);
            if (index == null && members.Length > 0)
            {
                foreach (var m in members)
                    missing.Add($"{Describe(m)} (game assembly not loaded)");
                return new ApiCompat(CompatStatus.Drifted, missing, members.Length, SurfaceManifest.Hash);
            }

            foreach (var m in members)
            {
                if (index == null || !index.TryGetValue(m.Type, out var type))
                {
                    missing.Add($"{m.Type} (type)");
                    continue;
                }

                if (m.Kind == "type")
                    continue;

                var flags = BindingFlags.Public | BindingFlags.NonPublic |
                            (m.Static ? BindingFlags.Static : BindingFlags.Instance);

                bool found;
                switch (m.Kind)
                {
                    case "field": found = type.GetField(m.Member, flags) != null; break;
                    case "property": found = type.GetProperty(m.Member, flags) != null; break;
                    case "method": found = HasMethod(type, m.Member, flags); break;
                    default: found = false; break;
                }

                if (!found)
                    missing.Add(Describe(m));
            }

            var status = missing.Count == 0 ? CompatStatus.Ok : CompatStatus.Drifted;
            return new ApiCompat(status, missing, members.Length, SurfaceManifest.Hash);
        }

        private static Dictionary<string, Type> BuildTypeIndex(Assembly asm)
        {
            if (asm == null) return null;
            var index = new Dictionary<string, Type>(StringComparer.Ordinal);
            foreach (var t in GameBind.SafeTypes(asm))
                if (t != null && !index.ContainsKey(t.Name))
                    index[t.Name] = t;
            return index;
        }

        private static bool HasMethod(Type type, string name, BindingFlags flags)
        {
            foreach (var m in type.GetMethods(flags))
                if (m.Name == name)
                    return true;
            return false;
        }

        private static string Describe(SurfaceMember m) =>
            m.Kind == "type" ? $"{m.Type} (type)" : $"{m.Type}.{m.Member} ({m.Kind})";
    }
}
