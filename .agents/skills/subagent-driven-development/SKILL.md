---
name: subagent-driven-development
description: |
  The in-session delegation engine /work uses to implement. One fresh subagent
  per task, parallel batches for independent tasks with disjoint file ownership
  dispatched in one message, and the fixed review order spec then quality then
  architecture then security. The orchestrator delegates and never implements.
---

# Subagent-driven development

## Purpose

Turn a scoped issue into a reviewed change without the orchestrator touching a
file. The orchestrator plans, dispatches subagents, and records verdicts; each
subagent does one job and hands back a result. This is the loop `/work` runs
during its implement and review phases. The roster it dispatches lives in
`.claude/agents/` and the rules it obeys live in `.agents/rules/`.

## The model

- The orchestrator MUST NOT edit files itself, because a change that skips the
  implementer also skips the review order that gates it (see
  `.agents/rules/subagent-workflow.md`).
- Every task gets a fresh subagent with a clean context. A subagent reused
  across tasks carries stale assumptions, so start each one from its roster
  prompt and the issue.
- Reviewers are read-only, because a reviewer that edits the work can no longer
  judge it independently.

## Roster

Each role maps to one file under `.claude/agents/`:

- `01-implementer` writes the code. Only this role writes production code.
- `02-spec-reviewer`, `03-code-quality-reviewer`, `04-architecture-validator`,
  `05-security-auditor` are the four gates, in that order.
- `06-test-runner` runs `make validate`; `07-debugger` diagnoses a red gate.

## The fixed order: implement, then review

One implement stage, then the four gates in sequence. The order is fixed:

    implementer -> spec -> quality -> architecture -> security

- Run the gates in that order, because each assumes the earlier one passed:
  spec fit before code quality, boundaries before the security audit.
- Each gate MUST end with exactly one verdict line whose first token is
  APPROVE, REQUEST-CHANGES, or REJECT, because the review-gate-tracker hook
  greps that token to record `gate_<name>` in the run state.
- A REQUEST-CHANGES or REJECT sends the work back to the implementer, then the
  gates re-run from spec. No gate is skipped on a re-run, because a later gate
  can be invalidated by the fix an earlier one required.

## Parallel batches

Independent tasks run in parallel; dependent tasks run in sequence.

- A parallel batch MUST dispatch in one message with disjoint file ownership,
  because two agents editing one file race and clobber each other.
- Batch size is roughly three to five tasks. Larger batches lose the thread and
  make a failure hard to attribute, so split them.
- Tasks in a batch MUST own non-overlapping paths. If two tasks would touch the
  same file, they are not independent; sequence them instead.

## Never parallelize

- Security review runs last and alone, because it needs the whole change in one
  view and a split view misses cross-file holes.
- Migrations run one at a time, because two concurrent schema or data
  migrations can interleave into a state neither one expected.

## Verbose output to files

- A subagent that produces long output (a full diff review, a test log) writes
  it to a file under the active run directory and returns a path plus a
  one-line summary, because piping a wall of text back through the orchestrator
  buries the signal.
- The one-line summary MUST carry the verdict token when the subagent is a
  gate, because the orchestrator and the tracker hook read that token, not the
  file.

## Prompt templates

Fill these and dispatch. They mirror the roster files one-to-one:

- `references/implementer-prompt.md` -> `01-implementer`
- `references/spec-reviewer-prompt.md` -> `02-spec-reviewer`
- `references/quality-reviewer-prompt.md` -> `03-code-quality-reviewer`

The architecture and security gates use their roster files directly; their
prompts are the role definitions in `.claude/agents/04-architecture-validator.md`
and `.claude/agents/05-security-auditor.md`.

## Anti-patterns

- The orchestrator implements instead of delegating.
- Running the four gates out of order, or skipping one on a re-run.
- Parallelizing security or a migration.
- A parallel batch whose tasks share a file.
- A gate that ends without a single greppable verdict line.

## References

- `.agents/rules/subagent-workflow.md` for the roster and stop conditions.
- `.agents/rules/challenge-protocol.md` for the reviewer posture.
- `.agents/rules/dependency-hierarchy.md` for the layer edges architecture checks.
- `references/workflow-contract.md` for the inputs, outputs, and invariants.
