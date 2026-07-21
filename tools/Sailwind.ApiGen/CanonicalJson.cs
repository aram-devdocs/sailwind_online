using System.Collections.Generic;
using System.Security.Cryptography;
using System.Text;

namespace Sailwind.ApiGen
{
    public static class ToolInfo
    {
        public const string Version = "0.1.0";
        public const string Name = "Sailwind.ApiGen";
        public static string Stamp => Name + "/" + Version;
    }

    public sealed class SurfaceHeader
    {
        public string AssemblyMvid;
        public string GameBuildId;
    }

    public sealed class SurfaceEntry
    {
        public string Type;
        public string Member;
        public string Kind;
        public bool Static;
        public string Signature;
    }

    public sealed class SurfaceDoc
    {
        public SurfaceHeader Header = new();
        public List<SurfaceEntry> Members = new();
    }

    /// <summary>
    /// Deterministic serializer for the surface manifest. The output is the
    /// compatibility token's preimage, so it must be byte-for-byte reproducible:
    /// sorted keys, 2-space indent, LF line endings, single trailing LF, UTF-8
    /// without BOM. Hand-rolled rather than delegated to a JSON library so the
    /// exact bytes never drift with a framework version.
    /// </summary>
    public static class CanonicalJson
    {
        private const string Nl = "\n";

        public static string Serialize(SurfaceDoc doc)
        {
            var sb = new StringBuilder();
            sb.Append('{').Append(Nl);

            // "header" (keys emitted in sorted order: assemblyMvid, gameBuildId, tool)
            sb.Append("  \"header\": {").Append(Nl);
            sb.Append("    \"assemblyMvid\": ").Append(Str(doc.Header.AssemblyMvid)).Append(',').Append(Nl);
            sb.Append("    \"gameBuildId\": ").Append(Str(doc.Header.GameBuildId)).Append(',').Append(Nl);
            sb.Append("    \"tool\": ").Append(Str(ToolInfo.Stamp)).Append(Nl);
            sb.Append("  },").Append(Nl);

            // "members" (sorted; each object's keys sorted: kind, member, signature, static, type)
            var members = SortMembers(doc.Members);
            if (members.Count == 0)
            {
                sb.Append("  \"members\": []").Append(Nl);
            }
            else
            {
                sb.Append("  \"members\": [").Append(Nl);
                for (int i = 0; i < members.Count; i++)
                {
                    var m = members[i];
                    sb.Append("    {").Append(Nl);
                    sb.Append("      \"kind\": ").Append(Str(m.Kind)).Append(',').Append(Nl);
                    sb.Append("      \"member\": ").Append(Str(m.Member)).Append(',').Append(Nl);
                    sb.Append("      \"signature\": ").Append(Str(m.Signature)).Append(',').Append(Nl);
                    sb.Append("      \"static\": ").Append(m.Static ? "true" : "false").Append(',').Append(Nl);
                    sb.Append("      \"type\": ").Append(Str(m.Type)).Append(Nl);
                    sb.Append("    }").Append(i == members.Count - 1 ? "" : ",").Append(Nl);
                }
                sb.Append("  ]").Append(Nl);
            }

            sb.Append('}').Append(Nl);
            return sb.ToString();
        }

        public static string Sha256Hex(string canonical)
        {
            var bytes = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false).GetBytes(canonical);
            var hash = SHA256.HashData(bytes);
            var sb = new StringBuilder(hash.Length * 2);
            foreach (var b in hash)
                sb.Append(b.ToString("x2"));
            return sb.ToString();
        }

        private static List<SurfaceEntry> SortMembers(List<SurfaceEntry> members)
        {
            var copy = new List<SurfaceEntry>(members);
            copy.Sort((a, b) =>
            {
                int c = string.CompareOrdinal(a.Type, b.Type);
                if (c != 0) return c;
                c = string.CompareOrdinal(a.Member, b.Member);
                if (c != 0) return c;
                c = string.CompareOrdinal(a.Kind, b.Kind);
                if (c != 0) return c;
                return string.CompareOrdinal(a.Signature, b.Signature);
            });
            return copy;
        }

        private static string Str(string s)
        {
            if (s == null) return "\"\"";
            var sb = new StringBuilder(s.Length + 2);
            sb.Append('"');
            foreach (var ch in s)
            {
                switch (ch)
                {
                    case '"': sb.Append("\\\""); break;
                    case '\\': sb.Append("\\\\"); break;
                    case '\b': sb.Append("\\b"); break;
                    case '\f': sb.Append("\\f"); break;
                    case '\n': sb.Append("\\n"); break;
                    case '\r': sb.Append("\\r"); break;
                    case '\t': sb.Append("\\t"); break;
                    default:
                        if (ch < 0x20)
                            sb.Append("\\u").Append(((int)ch).ToString("x4"));
                        else
                            sb.Append(ch);
                        break;
                }
            }
            sb.Append('"');
            return sb.ToString();
        }
    }
}
