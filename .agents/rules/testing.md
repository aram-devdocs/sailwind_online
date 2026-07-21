---
description: Test-driven development, the game-free vs game-coupled suite split, and never weakening a test to go green. The gate is make validate.
alwaysApply: true
---

# Testing

Write the failing test first. It proves the test can fail and pins the behavior
before the code exists.

- A behavior change MUST start with a test that fails for the right reason,
  because a test written after the code only confirms what was already built,
  not what was intended.
- You MUST NOT weaken, skip, disable, or delete a test, rule, or hook to reach
  green, because the gate exists to catch exactly the failure in front of you.
  Fix the code, or if the test is genuinely wrong, fix the test in a commit that
  says why.
- Game-free tests (the `packages/` suites and their projects under `tests/`,
  plus `cargo test`) MUST run in CI with no game IP present, because that is the
  suite that gates every push.
- Tests that need real game members belong to the game-coupled suite
  (`tests/Sailwind.Api.SurfaceTests` against `Sailwind.Api.Adapters`) and MUST
  NOT be mixed into a game-free project, because a game reference there breaks
  the CI build for everyone.
- The canonical gate is `make validate`; it MUST pass locally before every
  push, because CI runs the same scripts and a red push wastes a round trip.
