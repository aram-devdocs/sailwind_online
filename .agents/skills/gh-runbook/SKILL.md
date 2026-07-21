---
name: gh-runbook
description: |
  Turn an audit, a scope, or a large issue into a parent runbook plus a set of
  grouped child issues. Produces a plan and a child-issue manifest (title, body,
  labels, milestone, blocked-by), then creates them with a script that dry-runs
  by default and only writes to GitHub when --apply is passed. Feeds /work.
---

# gh-runbook

## Purpose

Break a big piece of work into issues the `/work` loop can pick up one at a
time. The output is a parent runbook (the plan) and a manifest of child issues
grouped and ordered by their blockers. The manifest becomes real GitHub issues
through `scripts/create-issues.sh`, which creates nothing until you pass
`--apply`. This skill feeds the backlog that `/work` drains.

## When to use

Use when a scope is too large for one issue: an audit with many findings, a
milestone with many tasks, or an epic that needs children. Do not use it to file
a single task; open that directly with the issue template.

## Process

1. Read the source scope: the audit, the parent issue, or the milestone. List
   the discrete units of work, because a runbook is only as good as the split.
2. Group the units. Each group is one child issue with a single acceptance
   criteria block. A unit that cannot be checked done is not yet an issue.
3. Order by dependency. Record `blocked-by: #N` for a child that cannot start
   until another closes, because `/work` skips an issue whose blockers are still
   open (see `.agents/skills/work/SKILL.md`).
4. Write the parent runbook as the parent issue body: context, the child list,
   and a done-when clause. Keep it in the issue, not a committed file, because
   `.agents/rules/documentation.md` bans PM artifacts in the repo.
5. Write the child manifest in the format `scripts/create-issues.sh` reads (see
   `references/example-manifest.md`). Reuse the section shape from
   `.github/ISSUE_TEMPLATE/task.md` so each body matches the board convention.
6. Dry-run the script and read every printed command. Only when the output is
   right, re-run with `--apply`.

## The manifest

One file, blocks separated by a line reading exactly `=== issue ===`. Per block:

- `title:` one line.
- `labels:` comma-separated (priority such as P1, plus an area label).
- `milestone:` the milestone name, or omit.
- `blocked-by:` comma-separated `#N`, or `None`. Appended to the body under a
  `## Blocked by` line so `/work` reads it.
- `body:` everything after this line until the next block or end of file.

`references/example-manifest.md` is a working sample and the script's default
input.

## Creating the issues

    bash scripts/create-issues.sh --manifest issues.md          # dry-run
    bash scripts/create-issues.sh --manifest issues.md --apply  # create

- The script MUST default to dry-run, because a manifest that files the wrong
  issues is expensive to unwind and a dry-run costs nothing.
- Run the dry-run and confirm each `would run` line before `--apply`, because
  `--apply` writes to the shared board.

## Anti-patterns

- Committing the runbook as a file in the repo instead of as the parent issue.
- A child issue with no checkable acceptance criteria.
- Running `--apply` without reading the dry-run first.
- Omitting `blocked-by` on a child that truly depends on another.

## References

- `scripts/create-issues.sh` for the creator.
- `references/example-manifest.md` for the manifest format.
- `references/workflow-contract.md` for the inputs, outputs, and invariants.
- `.github/ISSUE_TEMPLATE/task.md` for the body shape to reuse.
- `.agents/skills/work/SKILL.md` for how the created issues get picked up.
