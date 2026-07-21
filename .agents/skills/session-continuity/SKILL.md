---
name: session-continuity
description: |
  Save and restore run state across a context compaction. Maintains a MEMORY.md
  whiteboard (current state, in progress, next, decisions, blockers) capped at
  200 lines under the active run directory. Documents the protocol the
  save-session (PreCompact) and context-loader (SessionStart) hooks already run.
---

# Session continuity

## Purpose

Keep a `/work` run on its feet across a compaction. Compaction drops detail, so
the run writes what it needs to a durable whiteboard before it happens and reads
that whiteboard back after. This skill documents the protocol; the hooks under
`.claude/hooks/` run it. Do not reimplement the hooks; keep the whiteboard they
read accurate.

## The whiteboard

One file, `MEMORY.md`, under the active run directory
(`.agents/runs/<run_id>/MEMORY.md`). It is capped at 200 lines, because a
whiteboard longer than that is a second context to lose, not a summary. Five
sections:

- Current state: the phase and what is done.
- In progress: the task and the exact files open right now.
- Next: the next action, concrete enough to start without re-deriving it.
- Decisions: choices already made and why, so they are not relitigated.
- Blockers: what is stuck and on what.

Keep each section to what the next session cannot recover from disk on its own.
The files are the source of truth; the whiteboard is the pointer into them.

## What the hooks do

Two hooks wire this protocol. Read them rather than duplicating them:

- `.claude/hooks/save-session.sh` runs on PreCompact. It refreshes the durable
  snapshot in `MEMORY.md` from the run state (`state.json`): the run id, the
  phase, the four gate verdicts, and the open-plan-item count. It is capped at
  200 lines and never blocks a compaction.
- `.claude/hooks/context-loader.sh` runs on SessionStart. It prints the read
  order: `AGENTS.md` and the applicable rules first, then the active run state,
  then `.agents/lessons-learned.md`. It is a nudge, not a gate.

Both are inert when no run is active, because the enforcing layer only applies
inside a `/work` run.

## Save protocol (before compaction)

Per `.agents/rules/compaction.md`, before compaction you MUST write current
state to the run's `MEMORY.md`, because the post-compaction session has only
what you wrote down. Mark each task and phase status and note which files are in
progress. The PreCompact hook refreshes the run-and-gate snapshot; you own the
five prose sections above.

## Restore protocol (after compaction)

Per `.agents/rules/compaction.md`, after compaction you MUST re-orient before
acting, because acting on a half-remembered task edits the wrong thing. In
order:

1. Read `AGENTS.md` and the `.agents/rules/` that scope the change.
2. Read `.agents/runs/active` to find the active run, then that run's
   `MEMORY.md` and `state.json`.
3. Read the in-progress files named in the whiteboard.

You MUST NOT resume implementation from memory alone, because the summary that
survives compaction is lossy and the files on disk are the truth.

## Run state contract

`state.json` under the run directory holds flat top-level string keys only
(`run_id`, `issue`, `phase`, `branch`, `worktree`, `pr`, `gate_spec`,
`gate_quality`, `gate_architecture`, `gate_security`, `plan_open`,
`updated_at`). Nested arrays or objects are forbidden, because the hooks read it
with grep and sed, not a JSON parser, and a nested value breaks that read.
`.agents/runs/active` is a one-line file naming the active run directory.

## Anti-patterns

- Resuming work from the compaction summary instead of re-reading the files.
- Letting `MEMORY.md` grow past 200 lines until it is its own liability.
- Duplicating the hook logic in the skill instead of pointing at the hooks.
- Adding a nested value to `state.json` that the grep-based hooks cannot read.

## References

- `.agents/rules/compaction.md` for the normative save-and-restore rule.
- `.claude/hooks/save-session.sh` for the PreCompact snapshot.
- `.claude/hooks/context-loader.sh` for the SessionStart read order.
- `references/workflow-contract.md` for the inputs, outputs, and invariants.
