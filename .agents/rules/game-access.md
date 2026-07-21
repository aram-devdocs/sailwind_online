---
globs: ["apps/**", "packages/api-adapters/**"]
---

# Game access

- Game members are reached only through the Sailwind.API generated seam,
  because the manifest and its contract test are what turn a game update into
  a failing test instead of a broken user install.
- Every game member you use MUST appear in the ApiGen codegen manifest, because
  a member outside the manifest has no drift check and fails silently on the
  next game update.
- Ad-hoc reflection into `Assembly-CSharp` and magic-string member names
  outside the generated output are banned, because they bypass the surface
  hash that guards compatibility.
- Game, Unity, and BepInEx DLLs are referenced `Private=false` and MUST NOT be
  copied into plugin output or any package, because game IP never leaves the
  local machine.
