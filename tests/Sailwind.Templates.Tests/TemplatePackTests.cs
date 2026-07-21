using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Text;
using System.Text.Json;
using Xunit;

namespace Sailwind.Templates.Tests
{
    /// <summary>
    /// Proves the `dotnet new sailwind-mod` template pack is well-formed and actually
    /// scaffolds a mod that consumes the public Sailwind.API surface. Game-free: the
    /// generated project is a net472 BepInEx plugin, but generating and inspecting it
    /// needs no game DLL, so CI can verify the template even though the sample it
    /// mirrors cannot compile without lib/.
    /// </summary>
    public sealed class TemplatePackTests
    {
        private const string ShortName = "sailwind-mod";
        private const string SourceName = "SailwindModTemplate";

        private static string RepoRoot()
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

            throw new InvalidOperationException("Could not locate the repo root (no global.json above the test output).");
        }

        private static string TemplatePackDir() =>
            Path.Combine(RepoRoot(), "templates", ShortName);

        // --- Test 1: the template config is a valid dotnet-new template declaring our short name.
        [Fact]
        public void TemplateConfig_IsWellFormed_AndDeclaresSailwindModShortName()
        {
            var json = Path.Combine(TemplatePackDir(), ".template.config", "template.json");
            Assert.True(File.Exists(json), $"template config missing at {json}");

            using var doc = JsonDocument.Parse(File.ReadAllText(json));
            var root = doc.RootElement;

            Assert.Equal(ShortName, ShortNameOf(root));
            Assert.Equal(SourceName, root.GetProperty("sourceName").GetString());
            Assert.False(string.IsNullOrWhiteSpace(root.GetProperty("identity").GetString()));
            Assert.False(string.IsNullOrWhiteSpace(root.GetProperty("name").GetString()));

            var tags = root.GetProperty("tags");
            Assert.Equal("project", tags.GetProperty("type").GetString());
            Assert.Equal("C#", tags.GetProperty("language").GetString());
        }

        // --- Test 2: the template content mirrors the sample's consumer shape.
        [Fact]
        public void TemplateContent_MirrorsSampleConsumerShape()
        {
            var packDir = TemplatePackDir();
            var csproj = Path.Combine(packDir, SourceName + ".csproj");
            var plugin = Path.Combine(packDir, "Plugin.cs");

            Assert.True(File.Exists(csproj), $"template csproj missing at {csproj}");
            Assert.True(File.Exists(plugin), $"template Plugin.cs missing at {plugin}");

            var csprojText = File.ReadAllText(csproj);
            Assert.Contains("net472", csprojText);
            Assert.Contains("Sailwind.Api.Abstractions", csprojText);
            // The mod reaches the game only through the API, so it must not REFERENCE the game
            // assembly (a comment may still name it). No <Reference>/<ProjectReference> to it.
            Assert.DoesNotContain("Include=\"Assembly-CSharp", csprojText);

            var pluginText = File.ReadAllText(plugin);
            Assert.Contains("using Sailwind.Api;", pluginText);
            Assert.Contains("BepInDependency", pluginText);        // hard-depends on the Sailwind.API host
            Assert.Contains("SailwindApi.Ready", pluginText);      // subscribes to the Ready signal
            Assert.Contains("SailwindApi.Clock", pluginText);      // reads the clock
            Assert.Contains("SailwindApi.Wind", pluginText);       // reads the wind
            Assert.Contains("SailwindApi.PlayerBoat", pluginText); // reads the player boat
        }

        // --- Test 3: `dotnet new` really instantiates the mod with the name substituted.
        [Fact]
        public void DotnetNew_ScaffoldsMod_WithNameSubstitutedAndApiUsage()
        {
            const string modName = "Acme.Harbor.Mod";
            var tmp = Path.Combine(Path.GetTempPath(), "sw-tmpl-" + Guid.NewGuid().ToString("N"));
            var cliHome = Path.Combine(tmp, "cli-home"); // isolate the template engine store from the user's machine
            var outDir = Path.Combine(tmp, "out");
            Directory.CreateDirectory(cliHome);
            Directory.CreateDirectory(outDir);

            var env = new Dictionary<string, string>
            {
                ["DOTNET_CLI_HOME"] = cliHome,
                ["DOTNET_NOLOGO"] = "1",
                ["DOTNET_CLI_TELEMETRY_OPTOUT"] = "1",
                ["DOTNET_SKIP_FIRST_TIME_EXPERIENCE"] = "1",
            };

            try
            {
                var install = RunDotnet(env, tmp, "new", "install", TemplatePackDir(), "--force");
                Assert.True(install.ExitCode == 0, "dotnet new install failed:\n" + install.Output);

                var create = RunDotnet(env, tmp, "new", ShortName, "--name", modName, "--output", outDir);
                Assert.True(create.ExitCode == 0, "dotnet new sailwind-mod failed:\n" + create.Output);

                var scaffoldedCsproj = Path.Combine(outDir, modName + ".csproj");
                var scaffoldedPlugin = Path.Combine(outDir, "Plugin.cs");
                Assert.True(File.Exists(scaffoldedCsproj), $"expected {scaffoldedCsproj}; produced:\n{string.Join("\n", Directory.GetFiles(outDir))}");
                Assert.True(File.Exists(scaffoldedPlugin), $"expected {scaffoldedPlugin}");

                var csprojText = File.ReadAllText(scaffoldedCsproj);
                var pluginText = File.ReadAllText(scaffoldedPlugin);

                // The source-name token is gone; the requested mod name took its place.
                Assert.DoesNotContain(SourceName, csprojText);
                Assert.DoesNotContain(SourceName, pluginText);
                Assert.Contains(modName, csprojText);
                Assert.Contains(modName, pluginText);

                // The scaffolded plugin consumes the public API and hard-depends on the host.
                Assert.Contains("using Sailwind.Api;", pluginText);
                Assert.Contains("SailwindApi.Ready", pluginText);
                Assert.Contains("com.example", pluginText); // default GUID prefix symbol applied
            }
            finally
            {
                RunDotnet(env, tmp, "new", "uninstall", TemplatePackDir());
                TryDelete(tmp);
            }
        }

        // dotnet-new templates may put shortName as a scalar or a single-element array; accept both.
        private static string ShortNameOf(JsonElement root)
        {
            var el = root.GetProperty("shortName");
            return el.ValueKind == JsonValueKind.Array ? el[0].GetString() : el.GetString();
        }

        private static (int ExitCode, string Output) RunDotnet(
            IReadOnlyDictionary<string, string> env, string workingDir, params string[] args)
        {
            var dotnet = Environment.GetEnvironmentVariable("DOTNET_HOST_PATH");
            if (string.IsNullOrEmpty(dotnet))
            {
                dotnet = "dotnet";
            }

            var psi = new ProcessStartInfo(dotnet)
            {
                WorkingDirectory = workingDir,
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
            };
            foreach (var a in args)
            {
                psi.ArgumentList.Add(a);
            }

            foreach (var kv in env)
            {
                psi.Environment[kv.Key] = kv.Value;
            }

            var sb = new StringBuilder();
            using var p = new Process { StartInfo = psi };
            p.OutputDataReceived += (_, e) => { if (e.Data != null) { lock (sb) { sb.AppendLine(e.Data); } } };
            p.ErrorDataReceived += (_, e) => { if (e.Data != null) { lock (sb) { sb.AppendLine(e.Data); } } };
            p.Start();
            p.BeginOutputReadLine();
            p.BeginErrorReadLine();
            p.WaitForExit();
            return (p.ExitCode, sb.ToString());
        }

        private static void TryDelete(string dir)
        {
            try
            {
                if (Directory.Exists(dir))
                {
                    Directory.Delete(dir, recursive: true);
                }
            }
            catch
            {
                // Best-effort cleanup of a temp directory; never fail the test on cleanup.
            }
        }
    }
}
