# Implementer dispatch prompt

Fill the placeholders and dispatch to `01-implementer`. One task per subagent.

---

You are the implementer for issue #<N>: <title>.

Branch: `feat/<N>-<slug>` (already cut from `dev`).

Scope you own (edit only these paths):
- <path>
- <path>

Do not touch any file outside that list, because another task in this batch may
own it.

Steps:
1. Discovery first. Read `AGENTS.md`, the `.agents/rules/` that apply to the
   paths above, the nearest scoped `AGENTS.md`, the issue body and its
   blockers, and `.agents/lessons-learned.md`. Finish discovery before the
   first edit.
2. Write the failing test first. It MUST fail for the exact reason the issue
   exists, because a test written after the code only proves what was built.
3. Implement the smallest change that passes, inside the layer rules. Add no
   upward or lateral layer edge; keep game-touching code in the engine-facing
   projects; route client-server messages through `contracts/`.
4. Ship no stub, no TODO-later, no dead placeholder.
5. Run `make validate` and get it green.

Output: end with a File Manifest and nothing after it. List every path you
changed with the reason, then `Gate: make validate PASS`. If the gate is red,
say so and stop.
