namespace Sailwind.Contracts
{
    /// <summary>
    /// Hand-authored protocol constants that mirror the values baked into the
    /// generated FlatBuffers code (SwProto). Kept minimal and dependency-free so
    /// both the net472 plugins and the net8 tools can reference one source of truth.
    /// </summary>
    public static class Protocol
    {
        /// <summary>Wire protocol version. Matches SwProto.ProtocolVersion.Current.</summary>
        public const ushort Version = 1;

        /// <summary>FlatBuffers file identifier for the root Envelope table.</summary>
        public const string FileIdentifier = "SWO0";
    }
}
