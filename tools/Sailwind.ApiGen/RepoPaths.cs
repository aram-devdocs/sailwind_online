using System;
using System.IO;

namespace Sailwind.ApiGen
{
    /// <summary>
    /// Resolves the repository root (the directory containing <c>global.json</c>)
    /// by walking up from the current directory, and derives every input/output
    /// path from it so the tool never depends on an absolute committed path.
    /// </summary>
    public sealed class RepoPaths
    {
        public string Root { get; }

        private RepoPaths(string root) => Root = root;

        public static RepoPaths Discover(string startDir = null)
        {
            var dir = new DirectoryInfo(startDir ?? Directory.GetCurrentDirectory());
            while (dir != null)
            {
                if (File.Exists(Path.Combine(dir.FullName, "global.json")))
                    return new RepoPaths(dir.FullName);
                dir = dir.Parent;
            }

            throw new InvalidOperationException(
                "Could not locate the repository root (no global.json found above " +
                $"'{startDir ?? Directory.GetCurrentDirectory()}').");
        }

        public string GameAssembly => Path.Combine(Root, "lib", "Assembly-CSharp.dll");
        public string GameBuildFile => Path.Combine(Root, "lib", "game-build.txt");

        public string GeneratedDir => Path.Combine(Root, "src", "Sailwind.API", "Generated");
        public string GameRefFile => Path.Combine(GeneratedDir, "GameRef.g.cs");
        public string SurfaceManifestFile => Path.Combine(GeneratedDir, "SurfaceManifest.g.cs");

        public string ManifestJson => Path.Combine(Root, "manifest", "api-surface.json");
        public string ManifestSha256 => Path.Combine(Root, "manifest", "api-surface.sha256");

        public string SurfaceContractFile =>
            Path.Combine(Root, "tests", "Sailwind.Api.SurfaceTests", "SurfaceContract.g.cs");
    }
}
