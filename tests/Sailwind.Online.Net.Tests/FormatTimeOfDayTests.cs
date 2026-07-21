using Sailwind.Online.Client.Net;
using Xunit;

namespace Sailwind.Online.Net.Tests
{
    /// <summary>The HUD's clock formatter: fraction-of-day to HH:MM, wrapping past 1.0.</summary>
    public sealed class FormatTimeOfDayTests
    {
        [Theory]
        [InlineData(0f, "00:00")]
        [InlineData(0.25f, "06:00")]
        [InlineData(0.5f, "12:00")]
        [InlineData(0.75f, "18:00")]
        [InlineData(1.5f, "12:00")]   // wraps: only the fractional part counts
        public void FormatTimeOfDay_MapsFractionToClock(float fraction, string expected)
        {
            Assert.Equal(expected, NetClient.FormatTimeOfDay(fraction));
        }
    }
}
