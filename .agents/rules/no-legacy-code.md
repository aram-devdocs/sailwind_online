---
description: No dead code. One implementation per concern, and removing a feature removes all of it, because dead code teaches the next agent the wrong pattern.
alwaysApply: true
---

# No legacy code

Agents copy whatever they read. Dead code is training data for the wrong
pattern.

- Commented-out code MUST NOT be committed, because git history already keeps
  the old version and a commented block reads as a suggestion to restore it.
- Every concern has exactly one implementation; a second parallel path MUST NOT
  survive a change, because two ways to do one thing means one is stale and
  callers cannot tell which.
- Removing a feature MUST remove all of it (the members, the tests, the config,
  the docs), because a half-removed feature leaves orphaned members that the
  next agent wires back up.
- Unreferenced members MUST NOT linger, because an unused private is a warning
  and warnings are errors under the gate.

Enforced by warnings-as-errors (`TreatWarningsAsErrors=true`,
`cargo clippy -- -D warnings`) which flags most dead members, and by review
which catches the rest. `make validate` runs both.
