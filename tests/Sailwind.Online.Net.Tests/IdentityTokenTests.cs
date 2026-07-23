using System;
using System.Text.RegularExpressions;
using Sailwind.Online.Client.Net;
using Xunit;

namespace Sailwind.Online.Net.Tests
{
    public sealed class IdentityTokenTests
    {
        [Fact]
        public void GetOrCreatePreservesConfiguredOpaqueTokenWithoutPersisting()
        {
            const string configuredToken = "  opaque-token-with-padding==  ";
            bool persistCalled = false;

            string token = IdentityToken.GetOrCreate(
                configuredToken,
                _ => persistCalled = true);

            Assert.Equal(configuredToken, token);
            Assert.False(persistCalled);
        }

        [Theory]
        [InlineData(null)]
        [InlineData("")]
        public void GetOrCreateGeneratesAndPersistsBase64UrlToken(string configuredToken)
        {
            string persistedToken = null;

            string token = IdentityToken.GetOrCreate(
                configuredToken,
                value => persistedToken = value);

            Assert.Equal(token, persistedToken);
            Assert.Equal(43, token.Length);
            Assert.Matches(new Regex("^[A-Za-z0-9_-]{43}$"), token);
        }

        [Fact]
        public void GetOrCreatePropagatesPersistenceException()
        {
            var expected = new InvalidOperationException("config write failed");

            InvalidOperationException actual = Assert.Throws<InvalidOperationException>(
                () => IdentityToken.GetOrCreate(string.Empty, _ => throw expected));

            Assert.Same(expected, actual);
        }
    }
}
