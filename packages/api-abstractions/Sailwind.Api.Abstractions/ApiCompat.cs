using System.Collections.Generic;

namespace Sailwind.Api
{
    public enum CompatStatus
    {
        Ok,
        Drifted,
    }

    /// <summary>
    /// Result of the runtime surface check: whether every embedded manifest member
    /// still exists in the loaded game assembly. Drift never throws — affected
    /// services degrade to "unavailable" so a game update fails the contract check,
    /// not the player.
    /// </summary>
    public sealed class ApiCompat
    {
        public CompatStatus Status { get; }
        public IReadOnlyList<string> Missing { get; }
        public int MemberCount { get; }
        public string Hash { get; }

        public bool IsOk => Status == CompatStatus.Ok;

        public ApiCompat(CompatStatus status, IReadOnlyList<string>? missing, int memberCount, string hash)
        {
            Status = status;
            Missing = missing ?? new List<string>();
            MemberCount = memberCount;
            Hash = hash;
        }
    }
}
