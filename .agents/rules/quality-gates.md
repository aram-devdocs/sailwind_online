---
alwaysApply: true
---

# Quality gates

- Warnings are errors. C# builds under `TreatWarningsAsErrors=true` and Rust
  runs `cargo clippy -- -D warnings`, because a tolerated warning is the first
  crack a loosened rule spreads from.
- You MUST NOT weaken, skip, disable, or delete a failing test, rule, or hook
  to reach green, because the gate exists to catch exactly the failure you are
  looking at.
- You MUST NOT ship stub functions, TODO-later comments, or dead placeholder
  types, because 100% strict from day one never re-tightens once loosened. A
  feature that belongs to a later milestone is omitted, not stubbed.
- `--no-verify` is forbidden in every situation, because a bypassed gate is a
  gate that does not exist. A PreToolUse hook blocks it; do not work around
  the hook.
- `make check` MUST pass locally before every push, because CI runs the same
  scripts and a red push wastes a round trip.
