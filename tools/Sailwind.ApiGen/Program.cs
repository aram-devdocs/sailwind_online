using System;
using System.IO;

namespace Sailwind.ApiGen
{
    /// <summary>
    /// Introspection codegen for the Sailwind game assembly.
    ///
    ///   (default)              verify the seed manifest against lib/Assembly-CSharp.dll,
    ///                          then emit GameRef.g.cs, api-surface.json + .sha256,
    ///                          SurfaceManifest.g.cs and SurfaceContract.g.cs.
    ///   --dump-type &lt;name&gt;     print a type's members to stdout (discovery aid).
    ///   --assembly &lt;path&gt;      override the game assembly path.
    /// </summary>
    public static class Program
    {
        public static int Main(string[] args)
        {
            try
            {
                var paths = RepoPaths.Discover();
                string assemblyPath = paths.GameAssembly;
                string dumpType = null;

                for (int i = 0; i < args.Length; i++)
                {
                    switch (args[i])
                    {
                        case "--assembly":
                            assemblyPath = Require(args, ref i, "--assembly");
                            break;
                        case "--dump-type":
                            dumpType = Require(args, ref i, "--dump-type");
                            break;
                        case "-h":
                        case "--help":
                            PrintUsage();
                            return 0;
                        default:
                            Console.Error.WriteLine($"unknown argument: {args[i]}");
                            PrintUsage();
                            return 2;
                    }
                }

                if (!File.Exists(assemblyPath))
                {
                    Console.Error.WriteLine(
                        $"game assembly not found: {assemblyPath}\n" +
                        "Run `make setup` (scripts/setup-game.ps1) to populate lib/ first.");
                    return 2;
                }

                using var game = GameAssembly.Load(assemblyPath);

                if (dumpType != null)
                {
                    Console.Write(game.DumpType(dumpType));
                    return 0;
                }

                return Generate(paths, game);
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine("ApiGen failed: " + ex.Message);
                return 1;
            }
        }

        private static int Generate(RepoPaths paths, GameAssembly game)
        {
            var drift = game.Verify(Seed.Members, out var resolved);
            if (drift.Count > 0)
            {
                Console.Error.WriteLine(
                    $"surface drift: {drift.Count} manifest entr{(drift.Count == 1 ? "y" : "ies")} " +
                    "no longer match the game assembly:");
                foreach (var d in drift)
                    Console.Error.WriteLine("  - " + d);
                Console.Error.WriteLine(
                    "Inspect the affected types with `--dump-type <name>` and fix the seed in Manifest.cs.");
                return 1;
            }

            string gameBuildId = ReadGameBuildId(paths.GameBuildFile);
            var doc = CodeEmitter.BuildDoc(gameBuildId, game.Mvid, resolved);
            string json = CanonicalJson.Serialize(doc);
            string hash = CanonicalJson.Sha256Hex(json);

            CodeEmitter.WriteManifest(paths, doc);
            CodeEmitter.WriteGameRef(paths, resolved);
            CodeEmitter.WriteSurfaceManifest(paths, doc, json, hash);
            CodeEmitter.WriteSurfaceContract(paths, doc);

            Console.WriteLine(
                $"ApiGen OK — {resolved.Count} member(s), surface {hash[..8]} " +
                $"(build {doc.Header.GameBuildId}, mvid {doc.Header.AssemblyMvid}).");
            return 0;
        }

        private static string ReadGameBuildId(string file)
        {
            if (!File.Exists(file)) return "unprovisioned";
            foreach (var line in File.ReadAllLines(file))
            {
                var t = line.Trim();
                if (t.Length > 0) return t;
            }
            return "unprovisioned";
        }

        private static string Require(string[] args, ref int i, string flag)
        {
            if (i + 1 >= args.Length)
                throw new ArgumentException($"{flag} requires a value");
            return args[++i];
        }

        private static void PrintUsage()
        {
            Console.Error.WriteLine(
                "Sailwind.ApiGen\n" +
                "  (no args)            verify seed manifest and emit generated surface artifacts\n" +
                "  --dump-type <name>   print a game type's members (discovery aid)\n" +
                "  --assembly <path>    override lib/Assembly-CSharp.dll\n" +
                "  -h | --help          show this help");
        }
    }
}
