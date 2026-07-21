using System.Collections.Generic;

namespace Sailwind.Architecture.Tests
{
    /// <summary>
    /// A metadata-only snapshot of one assembly: its simple name, the simple names of
    /// every assembly it references, and the simple names of every type it declares.
    /// The rule validators operate solely on this record, so the exact same rules run
    /// against DLLs discovered on disk (<see cref="AssemblyScanner"/>) and against the
    /// hand-built synthetic fixtures that prove each rule bites.
    /// </summary>
    public sealed class AssemblyModel
    {
        public AssemblyModel(
            string name,
            IReadOnlyList<string> references,
            IReadOnlyList<string> declaredTypeNames)
        {
            Name = name;
            References = references;
            DeclaredTypeNames = declaredTypeNames;
        }

        /// <summary>The assembly's simple name, e.g. "Sailwind.Online.Net".</summary>
        public string Name { get; }

        /// <summary>Simple names of every assembly this one references (game, package, framework, all of them).</summary>
        public IReadOnlyList<string> References { get; }

        /// <summary>Simple names of every type this assembly declares (nested included; compiler-generated excluded).</summary>
        public IReadOnlyList<string> DeclaredTypeNames { get; }
    }
}
