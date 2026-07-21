---
description: Read the relevant code and rules before editing, and reuse an existing pattern instead of inventing one, because agents copy whatever they see.
alwaysApply: true
---

# Discovery first

Look before you write. The next agent copies whatever pattern you leave behind,
so leave the one that already exists.

- You MUST read the code you are about to change and the rules under
  `.agents/rules/` that scope it before editing, because a change made without
  reading its neighbors breaks an invariant you never saw.
- You MUST search for an existing utility, helper, or pattern before writing a
  new one (grep the packages, glob for similar files), because a second
  implementation of the same thing is dead code by the next commit.
- When a pattern already exists you SHOULD extend or reuse it rather than invent
  a parallel one, because a divergent pattern teaches the next agent that both
  are acceptable and the codebase splits.
- If no fitting pattern exists, you SHOULD note that in the run before inventing,
  because an intentional new pattern is reviewable and an accidental one is not.
