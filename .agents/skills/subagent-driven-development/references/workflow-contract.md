# Workflow contract: subagent-driven development

The machine-checkable shape of this skill. The validator confirms the files and
sections named here exist; this document states the behavior they encode.

## Inputs

- One scoped GitHub issue with a branch already cut from `dev`.
- The active run directory (`.agents/runs/<run_id>/`) for verbose output files.
- The roster under `.claude/agents/` and the rules under `.agents/rules/`.

## Outputs

- A File Manifest from the implementer.
- Four verdict lines, one per gate, first token APPROVE, REQUEST-CHANGES, or
  REJECT.
- For long results, a file under the run directory plus a one-line summary that
  carries the verdict token.

## Invariants

- The orchestrator writes no source file; only `01-implementer` does.
- Gate order is spec, quality, architecture, security, with no skips and no
  reordering, including on a re-run after REQUEST-CHANGES.
- A parallel batch dispatches in one message and its tasks own disjoint paths.
- Security review and migrations never run in parallel.
- Batch size stays near three to five tasks.

## Roster mapping

| Prompt template                      | Roster file                              |
|--------------------------------------|------------------------------------------|
| references/implementer-prompt.md     | .claude/agents/01-implementer.md         |
| references/spec-reviewer-prompt.md   | .claude/agents/02-spec-reviewer.md       |
| references/quality-reviewer-prompt.md| .claude/agents/03-code-quality-reviewer.md |

The architecture and security gates dispatch from their roster files directly.

## Failure handling

- A REQUEST-CHANGES or REJECT returns the work to the implementer; the gates
  then re-run from spec.
- A red `make validate` goes to `07-debugger`, which fixes the cause and adds a
  regression test before the gates resume.
