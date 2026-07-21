using System.IO;
using Xunit;

namespace Sailwind.Api.SurfaceManifest.Tests
{
    /// <summary>
    /// Game-free integrity gate for the API surface manifest hash chain. The
    /// runtime SurfaceCheck already warns-not-crashes at load; the gap this pins
    /// is that nothing in cloud CI verified the sha256 chain, so a hand-edited
    /// manifest/api-surface.json or a stale embedded hash would have passed. Each
    /// fact recomputes the canonical SHA-256 of the committed manifest exactly as
    /// Sailwind.ApiGen does and asserts the three committed values agree.
    /// </summary>
    public sealed class SurfaceManifestIntegrityTests
    {
        [Fact]
        public void RecomputedManifestHash_Equals_CommittedSha256File()
        {
            Assert.Equal(
                SurfaceManifestIntegrity.CommittedSha256File(),
                SurfaceManifestIntegrity.RecomputedManifestHash());
        }

        [Fact]
        public void RecomputedManifestHash_Equals_EmbeddedConstant()
        {
            Assert.Equal(
                SurfaceManifestIntegrity.EmbeddedHashConstant(),
                SurfaceManifestIntegrity.RecomputedManifestHash());
        }

        [Fact]
        public void CommittedSha256File_Equals_EmbeddedConstant()
        {
            Assert.Equal(
                SurfaceManifestIntegrity.EmbeddedHashConstant(),
                SurfaceManifestIntegrity.CommittedSha256File());
        }

        // Proves the recomputation actually bites: a hand-edited manifest (here a
        // single renamed member) no longer matches the committed hash chain, so the
        // positive facts above are a real gate and not a vacuous pass.
        [Fact]
        public void TamperedManifest_BreaksHashChain()
        {
            var canonical = SurfaceManifestIntegrity.Canonicalize(
                File.ReadAllText(SurfaceManifestIntegrity.ManifestJsonPath));
            var tampered = canonical.Replace("\"Wind\"", "\"Windy\"");

            // Guard: the edit must have actually changed the canonical bytes,
            // otherwise the negative assertions below would be meaningless.
            Assert.NotEqual(canonical, tampered);

            var tamperedHash = SurfaceManifestIntegrity.ComputeCanonicalSha256(tampered);
            Assert.NotEqual(SurfaceManifestIntegrity.CommittedSha256File(), tamperedHash);
            Assert.NotEqual(SurfaceManifestIntegrity.EmbeddedHashConstant(), tamperedHash);
        }
    }
}
