using System;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using System.Text.RegularExpressions;

namespace Sailwind.Api.SurfaceManifest.Tests
{
    /// <summary>
    /// Recomputes the canonical SHA-256 of <c>manifest/api-surface.json</c> exactly
    /// as <c>Sailwind.ApiGen.CanonicalJson</c> does (LF line endings, a single
    /// trailing LF, UTF-8 without BOM) and reads back the committed
    /// <c>api-surface.sha256</c> file plus the embedded <c>SurfaceManifest.Hash</c>
    /// literal. It only reads committed repo files and a source literal — no
    /// <c>lib/</c>, no game DLL — so it belongs to the game-free CI tier.
    /// </summary>
    public static class SurfaceManifestIntegrity
    {
        /// <summary>Repo root = the directory containing <c>global.json</c>, found by
        /// walking up from the test binary, mirroring ApiGen's RepoPaths.Discover.</summary>
        public static string RepoRoot()
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

            throw new InvalidOperationException(
                "Could not locate the repository root (no global.json above " +
                AppContext.BaseDirectory + ").");
        }

        public static string ManifestJsonPath =>
            Path.Combine(RepoRoot(), "manifest", "api-surface.json");

        public static string ManifestSha256Path =>
            Path.Combine(RepoRoot(), "manifest", "api-surface.sha256");

        public static string SurfaceManifestSourcePath => Path.Combine(
            RepoRoot(), "packages", "api-adapters", "Sailwind.Api.Adapters",
            "Generated", "SurfaceManifest.g.cs");

        /// <summary>Reduces on-disk text to the canonical hash preimage: normalizes
        /// CRLF/CR to LF and ends with exactly one trailing LF, the exact whitespace
        /// contract CanonicalJson guarantees, so the recomputation matches even if
        /// git checked the file out with platform-native line endings.
        /// (File.ReadAllText already strips any UTF-8 BOM.)</summary>
        public static string Canonicalize(string text)
        {
            text = text.Replace("\r\n", "\n").Replace("\r", "\n");
            return text.TrimEnd('\n') + "\n";
        }

        public static string ComputeCanonicalSha256(string canonical)
        {
            var bytes = new UTF8Encoding(encoderShouldEmitUTF8Identifier: false).GetBytes(canonical);
            var hash = SHA256.HashData(bytes);
            var sb = new StringBuilder(hash.Length * 2);
            foreach (var b in hash)
            {
                sb.Append(b.ToString("x2"));
            }

            return sb.ToString();
        }

        public static string RecomputedManifestHash() =>
            ComputeCanonicalSha256(Canonicalize(File.ReadAllText(ManifestJsonPath)));

        public static string CommittedSha256File() =>
            File.ReadAllText(ManifestSha256Path).Trim();

        public static string EmbeddedHashConstant()
        {
            var source = File.ReadAllText(SurfaceManifestSourcePath);
            var match = Regex.Match(source, "public const string Hash = \"(?<hash>[0-9a-fA-F]+)\";");
            if (!match.Success)
            {
                throw new InvalidOperationException(
                    "Could not find `public const string Hash = \"...\";` in " +
                    SurfaceManifestSourcePath);
            }

            return match.Groups["hash"].Value;
        }
    }
}
