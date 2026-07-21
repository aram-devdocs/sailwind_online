using System;
using System.Reflection;
using UnityEngine;

namespace Sailwind.Api.Runtime
{
    /// <summary>
    /// Plain-reflection binder over the loaded <c>Assembly-CSharp</c>. Reflection
    /// sees private members and needs no Reflection.Emit (the game reports
    /// <c>Supports SRE: False</c>). Every lookup returns null on miss so adapters
    /// degrade instead of throwing.
    ///
    /// Member-name candidates passed here are discovery seeds: the verified names
    /// land in <c>GameRef.g.cs</c> after <c>make codegen</c> runs ApiGen against the
    /// real DLL. Until then, an adapter that resolves nothing simply reports its
    /// service unavailable.
    /// </summary>
    internal static class GameBind
    {
        private const BindingFlags Any =
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance | BindingFlags.Static;

        public static Assembly GameAssembly
        {
            get
            {
                foreach (var a in AppDomain.CurrentDomain.GetAssemblies())
                    if (a.GetName().Name == "Assembly-CSharp")
                        return a;
                return null;
            }
        }

        public static Type[] SafeTypes(Assembly asm)
        {
            if (asm == null) return Array.Empty<Type>();
            try { return asm.GetTypes(); }
            catch (ReflectionTypeLoadException ex) { return ex.Types ?? Array.Empty<Type>(); }
        }

        public static Type Resolve(string simpleName)
        {
            if (string.IsNullOrEmpty(simpleName)) return null;
            foreach (var t in SafeTypes(GameAssembly))
                if (t != null && t.Name == simpleName)
                    return t;
            return null;
        }

        public static object Instance(Type t)
        {
            if (t == null) return null;
            try { return UnityEngine.Object.FindObjectOfType(t); }
            catch { return null; }
        }

        public static MethodInfo Method(Type t, params string[] names)
        {
            if (t == null) return null;
            var all = t.GetMethods(Any);
            foreach (var n in names)
                foreach (var m in all)
                    if (m.Name == n)
                        return m;
            return null;
        }

        /// <summary>Resolves the first matching field/property and returns a reader
        /// closure (pass null as the instance for static members).</summary>
        public static Func<object, object> Getter(Type t, params string[] names)
        {
            if (t == null) return null;
            foreach (var n in names)
            {
                var f = t.GetField(n, Any);
                if (f != null) return o => f.GetValue(o);
                var p = t.GetProperty(n, Any);
                if (p != null && p.CanRead) return o => p.GetValue(o);
            }
            return null;
        }
    }
}
