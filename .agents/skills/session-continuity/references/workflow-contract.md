# Workflow contract: session-continuity

The machine-checkable shape of this skill. The validator confirms the files and
sections named here exist and that the skill points at the hooks rather than
reimplementing them.

## Inputs

- The active run directory named by `.agents/runs/active`.
- That run's `state.json` (flat string keys) and `MEMORY.md` whiteboard.
- `AGENTS.md`, the applicable `.agents/rules/`, and
  `.agents/lessons-learned.md`.

## Outputs

- A `MEMORY.md` whiteboard under the run directory, at most 200 lines, with the
  five sections: current state, in progress, next, decisions, blockers.

## Invariants

- `MEMORY.md` stays at or under 200 lines.
- The whiteboard points into the on-disk files; the files stay the source of
  truth. Work never resumes from the compaction summary alone.
- `state.json` holds flat top-level string keys only; no nested array or object,
  because the hooks read it with grep and sed.
- This skill references the hooks; it does not duplicate their logic.

## Hook mapping

| Event        | Hook                             | Job                                   |
|--------------|----------------------------------|---------------------------------------|
| PreCompact   | .claude/hooks/save-session.sh    | refresh the run-and-gate snapshot     |
| SessionStart | .claude/hooks/context-loader.sh  | print the read order to re-orient     |

Both hooks are inert when no run is active.

## Restore order

1. `AGENTS.md` and the scoping rules.
2. `.agents/runs/active`, then the run's `MEMORY.md` and `state.json`.
3. The in-progress files named in the whiteboard.
