# Workflow contract: gh-runbook

The machine-checkable shape of this skill. The validator confirms the files and
sections named here exist and that the script is safe by default.

## Inputs

- A source scope: an audit, a parent issue, or a milestone.
- A child-issue manifest in the block format `scripts/create-issues.sh` reads.
- `.github/ISSUE_TEMPLATE/task.md` for the body shape to reuse.

## Outputs

- A parent runbook written as the parent GitHub issue body (not a repo file).
- Child GitHub issues, each with a title, labels, optional milestone, a
  `blocked-by` line in the body, and checkable acceptance criteria.

## Invariants

- `scripts/create-issues.sh` creates nothing without `--apply`; the default run
  is a dry-run that prints the `gh` commands it would run.
- The runbook is never committed as a file, because PM artifacts do not belong
  in the repo.
- Every child issue carries at least one checkable acceptance criterion.
- `blocked-by` records real ordering so `/work` skips a blocked issue.

## Manifest keys

| Key         | Shape                          | Effect                              |
|-------------|--------------------------------|-------------------------------------|
| title       | one line                       | `gh issue create --title`           |
| labels      | comma-separated                | one `--label` per value             |
| milestone   | milestone name (optional)      | `gh issue create --milestone`       |
| blocked-by  | comma-separated `#N` or None   | appended to body under `## Blocked by` |
| body        | lines until next block or EOF  | `gh issue create --body-file`       |

## Safety

- Dry-run is the default and prints every command without side effects.
- `--apply` requires `gh` on PATH and writes to the board; run it only after
  reviewing the dry-run output.
