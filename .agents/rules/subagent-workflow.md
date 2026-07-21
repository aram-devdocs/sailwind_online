---
description: The subagent roster, the fixed review order, parallel-batch rules with disjoint file ownership, and the red flags that stop work.
alwaysApply: true
---

# Subagent workflow

The orchestrator delegates; it does not implement.

## Roster

- One implementer writes code.
- Four reviewers run in a fixed order: spec, then quality, then architecture,
  then security. Each emits a verdict line whose first token is APPROVE,
  REQUEST-CHANGES, or REJECT. No verdict means no pass.
- A test-runner runs `make validate`; a debugger diagnoses failures.

## Rules

- The orchestrator MUST NOT edit files itself, because a change that skips the
  implementer also skips the review order that gates it.
- Reviewers are read-only (no Write, no Edit), because a reviewer that edits the
  work can no longer judge it independently.
- Parallel batches MUST dispatch in one message with disjoint file ownership,
  because two agents editing one file race and clobber each other.
- Security review and migrations MUST NOT be parallelized, because both need the
  whole change in view and a split view misses cross-file holes.

## Red flags, stop and correct

- Skipping a review gate or running the four out of order.
- Skipping TDD (code before the failing test).
- Passing `--no-verify` or working around the block-no-verify hook.
- Implementing in the orchestrator instead of delegating.
- Editing or overwriting a reviewer's verdict.
