using System;
using System.Collections.Generic;
using System.Linq;
using System.Text;
using Mono.Cecil;

namespace Sailwind.ApiGen
{
    public sealed class ResolvedEntry
    {
        public GameMember Source;
        public string Signature;
    }

    /// <summary>
    /// Cecil view over the raw (non-publicized) game assembly. Cecil enumerates
    /// private members without loading Unity dependencies, which reflection-only
    /// load cannot do for this assembly.
    /// </summary>
    public sealed class GameAssembly : IDisposable
    {
        private readonly AssemblyDefinition _asm;
        private readonly Dictionary<string, List<TypeDefinition>> _bySimpleName;

        public Guid Mvid => _asm.MainModule.Mvid;

        private GameAssembly(AssemblyDefinition asm)
        {
            _asm = asm;
            _bySimpleName = new Dictionary<string, List<TypeDefinition>>(StringComparer.Ordinal);
            foreach (var t in AllTypes(asm.MainModule.Types))
            {
                if (!_bySimpleName.TryGetValue(t.Name, out var list))
                    _bySimpleName[t.Name] = list = new List<TypeDefinition>();
                list.Add(t);
            }
        }

        public static GameAssembly Load(string path) =>
            new(AssemblyDefinition.ReadAssembly(path));

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

        private TypeDefinition FindType(string simpleName)
        {
            if (_bySimpleName.TryGetValue(simpleName, out var list) && list.Count > 0)
                return list[0];
            return null;
        }

        /// <summary>
        /// Verifies every seed entry against the assembly. Collects <em>all</em>
        /// drift and returns it so a single failure enumerates every broken
        /// dependency; on success, populates <paramref name="resolved"/> with
        /// canonical signatures.
        /// </summary>
        public IReadOnlyList<string> Verify(
            IEnumerable<GameMember> seed,
            out List<ResolvedEntry> resolved)
        {
            var drift = new List<string>();
            resolved = new List<ResolvedEntry>();

            foreach (var m in seed)
            {
                var type = FindType(m.Type);
                if (type == null)
                {
                    drift.Add($"type '{m.Type}' not found");
                    continue;
                }

                switch (m.Kind)
                {
                    case Kinds.Type:
                        resolved.Add(new ResolvedEntry { Source = m, Signature = type.FullName });
                        break;

                    case Kinds.Field:
                    {
                        var f = type.Fields.FirstOrDefault(x => x.Name == m.Member);
                        if (f == null) { drift.Add($"{m.Type}.{m.Member} (field) not found"); break; }
                        if (f.IsStatic != m.Static)
                            drift.Add($"{m.Type}.{m.Member} (field) static={f.IsStatic}, expected {m.Static}");
                        else
                            resolved.Add(new ResolvedEntry { Source = m, Signature = FieldSig(type, f) });
                        break;
                    }

                    case Kinds.Property:
                    {
                        var p = type.Properties.FirstOrDefault(x => x.Name == m.Member);
                        if (p == null) { drift.Add($"{m.Type}.{m.Member} (property) not found"); break; }
                        bool isStatic = (p.GetMethod ?? p.SetMethod)?.IsStatic ?? false;
                        if (isStatic != m.Static)
                            drift.Add($"{m.Type}.{m.Member} (property) static={isStatic}, expected {m.Static}");
                        else
                            resolved.Add(new ResolvedEntry { Source = m, Signature = PropertySig(type, p) });
                        break;
                    }

                    case Kinds.Method:
                    {
                        var overloads = type.Methods.Where(x => x.Name == m.Member).ToList();
                        if (overloads.Count == 0) { drift.Add($"{m.Type}.{m.Member}(...) (method) not found"); break; }
                        var chosen = overloads.FirstOrDefault(x => x.IsStatic == m.Static);
                        if (chosen == null)
                            drift.Add($"{m.Type}.{m.Member}(...) (method) has no overload with static={m.Static}");
                        else
                            resolved.Add(new ResolvedEntry { Source = m, Signature = MethodSig(type, chosen) });
                        break;
                    }

                    default:
                        drift.Add($"{m.Type}.{m.Member}: unknown kind '{m.Kind}'");
                        break;
                }
            }

            return drift;
        }

        public string DumpType(string simpleName)
        {
            var type = FindType(simpleName);
            if (type == null)
                return $"type '{simpleName}' not found in {_asm.Name.Name}";

            var sb = new StringBuilder();
            sb.AppendLine($"// {type.FullName}   (module mvid {Mvid})");
            sb.AppendLine($"// base: {type.BaseType?.FullName ?? "<none>"}");

            sb.AppendLine("// fields:");
            foreach (var f in type.Fields.OrderBy(x => x.Name, StringComparer.Ordinal))
                sb.AppendLine($"  field   {(f.IsStatic ? "static " : "")}{FieldSig(type, f)}");

            sb.AppendLine("// properties:");
            foreach (var p in type.Properties.OrderBy(x => x.Name, StringComparer.Ordinal))
            {
                bool isStatic = (p.GetMethod ?? p.SetMethod)?.IsStatic ?? false;
                sb.AppendLine($"  property {(isStatic ? "static " : "")}{PropertySig(type, p)}");
            }

            sb.AppendLine("// methods:");
            foreach (var mth in type.Methods.OrderBy(x => x.Name, StringComparer.Ordinal))
                sb.AppendLine($"  method  {(mth.IsStatic ? "static " : "")}{MethodSig(type, mth)}");

            return sb.ToString();
        }

        private static string FieldSig(TypeDefinition t, FieldDefinition f) =>
            $"{f.FieldType.FullName} {t.FullName}.{f.Name}";

        private static string PropertySig(TypeDefinition t, PropertyDefinition p) =>
            $"{p.PropertyType.FullName} {t.FullName}.{p.Name}";

        private static string MethodSig(TypeDefinition t, MethodDefinition m)
        {
            var ps = string.Join(", ", m.Parameters.Select(x => x.ParameterType.FullName));
            return $"{m.ReturnType.FullName} {t.FullName}.{m.Name}({ps})";
        }

        public void Dispose() => _asm?.Dispose();
    }
}
