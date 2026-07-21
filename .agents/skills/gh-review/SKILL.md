---
name: gh-review
description: |
  A local pull-request review that mirrors the CI review criteria so self-review
  and CI cannot drift: build, lint, tests, architecture, and the leak scan, run
  through the same scripts CI runs. Works on a PR number or on the local diff
  dev...HEAD. Sorts the diff into file buckets and ends with exactly one verdict.
---

# gh-review

## Purpose

Review a change against the same gates CI enforces, before CI runs or before a
human merges. The point is no drift: this skill calls the same scripts the
`ci.yml` workflow calls, so a local APPROVE means the same thing a green CI run
means. It ends with one machine-readable verdict line.

## When to use

Use to self-review a branch before opening or merging a PR, or to review someone
else's PR locally. Do not use it to replace CI; use it so CI has nothing left to
catch.

## Modes

- PR number: check out the PR head, then review its diff against `dev`.

      gh pr checkout <N>

- Local diff: review the working branch directly.

      git diff dev...HEAD

Pick one mode and state it at the top of the review, because the reader needs to
know what range the verdict covers.

## Mirror the CI gates

Run the same checks CI runs, from the repo root. Do not paraphrase them; call
the scripts, because a hand-rolled check drifts from the one CI trusts:

1. Canonical gate: `make validate` (conventions, leak scan, game-IP guard,
   game-free build, every test suite, the architecture DAG, contract drift).
2. Leak scan on its own if you need the detail: `bash scripts/check-no-leak.sh`.
3. Game-IP guard on its own: `bash scripts/guard-no-game-ip.sh`.
4. Architecture DAG: `dotnet test tests/Sailwind.Architecture.Tests` and the
   Rust crate-DAG test under `cargo test --workspace`.

A red result from any of these is at least REQUEST-CHANGES, because CI will fail
the same way.

## File-bucket taxonomy

Sort every changed path into one bucket and review each bucket for what it is
prone to break:

- contracts: `contracts/fbs/**` and generated output. Check the schema and its
  regenerated code landed together, because a schema change without regenerated
  code is a wire break.
- api: `packages/Sailwind.Api.*`, `apps/Sailwind.API`. Check the surface hash
  and that game members are reached only through the generated seam.
- server: `server/**` Rust crates. Check wire-input bounds and the crate DAG.
- client: `apps/Sailwind.Online.Client`, `packages/Sailwind.Online.*`. Check it
  shares nothing with the server but `contracts/`.
- infra: `.github/**`, `scripts/**`, `Makefile`, `.agents/**`, `.claude/**`.
  Check no gate was weakened and no hook bypassed.
- docs: `docs/**`, `*.md`. Check RFC 2119 usage, the banned vocabulary, and no
  em dash.

## Blocker-class findings

Any one of these is a blocker: raise the verdict to at least REQUEST-CHANGES and
name the `path:line`, because each one breaks a gate the whole team relies on:

- A suppressed or downgraded warning (a `#pragma warning disable`, an
  `#[allow(...)]`, a `<NoWarn>`), because warnings are errors here.
- A hand-edited generated file, because the next regeneration silently reverts
  it and the drift check fails.
- A game, Unity, or BepInEx reference outside `apps/**` and the api-adapters
  package, because that reference breaks the game-free CI build.
- A leak term: any private organization, client, product, repository, or person
  name, because this repository is public.

## Verdict

End with exactly one line whose first token is the verdict:

    APPROVE - gates green, no blocker-class finding.
    REQUEST-CHANGES - <path:line>: <what and why>.
    REJECT - <the change is wrong at its root>.

Exactly one verdict line, because a review with two verdicts or none cannot be
read as a gate result.

## Anti-patterns

- Approving without running `make validate`.
- Paraphrasing a CI check instead of calling its script, which is how drift
  starts.
- Listing a style nit while a blocker-class finding goes unnamed.
- Ending with no verdict line, or with more than one.

## References

- `.github/workflows/ci.yml` for the gates this mirrors.
- `.agents/rules/quality-gates.md` for warnings-as-errors and no-bypass.
- `.agents/rules/dependency-hierarchy.md` for the layer edges to check.
- `.agents/rules/testing.md` for the game-free suite split.
- `references/workflow-contract.md` for the inputs, outputs, and invariants.
