namespace Sailwind.Api
{
    /// <summary>
    /// One entry of the embedded surface manifest. The generated
    /// <c>SurfaceManifest.g.cs</c> materializes these; the runtime surface check
    /// reflects each one against the loaded game assembly.
    /// </summary>
    public readonly struct SurfaceMember
    {
        public readonly string Type;
        public readonly string Member;
        public readonly string Kind;
        public readonly bool Static;

        public SurfaceMember(string type, string member, string kind, bool isStatic)
        {
            Type = type;
            Member = member;
            Kind = kind;
            Static = isStatic;
        }
    }
}
