---
name: work
version: 1.0.0
description: |
  Self-directed issue loop for this repository. Checks repo and board state,
  picks the highest-priority unblocked GitHub issue, branches from dev,
  implements with tests, opens a PR to dev, records lessons learned, and
  reports. One issue per invocation.
---

# Work

## Purpose

Drive one GitHub issue from open to a green pull request against `dev`
without human steering. The issue list is the backlog; this skill is the
executor.

## Preconditions

Stop and report instead of proceeding when any of these fail:

- `gh auth status` shows the aram-devdocs account.
- `AGENTS.md` has been read this session.
- The worktree state from step 1 has been reconciled.

## Process

### 1. Orient (re-entry check)

Never assume a fresh world. A previous run may have died mid-task, and a
worktree that is complete but uncommitted is the classic failure mode. In
order:

    git fetch origin
    git status --porcelain
    git branch --show-current
    gh pr list --base dev --state open --json number,title,headRefName,statusCheckRollup

- Dirty worktree on a `feat/*` branch: inspect the diff against the matching
  issue, then finish, commit, or discard deliberately. Never blind-reset.
- An open PR whose branch matches an issue you would pick: that issue is
  taken. Resume it only when its CI is red and nothing else is active;
  otherwise pick the next issue.
- Clean state: `git switch dev && git pull --ff-only`.

### 2. Select an issue

    gh issue list --state open --limit 100 --json number,title,labels,milestone,assignees

Selection order, applied in sequence:

1. Lowest milestone (M0 before M1, and so on). No milestone sorts last.
2. Priority label: P0, then P1, then P2.
3. Lowest issue number.

Skip an issue when any of these hold:

- It carries the `blocked` label.
- Its body says `Blocked by #N` and issue N is still open
  (`gh issue view N --json state`).
- It is assigned and shows activity newer than 24 hours.
- An open PR already references it.

If nothing is selectable, report that and stop.

### 3. Claim

    gh issue edit <N> --add-assignee @me
    gh issue comment <N> --body "Picking this up. Branch: feat/<N>-<slug>"

### 4. Branch

    git switch dev
    git pull --ff-only
    git switch -c feat/<N>-<slug>

`<slug>` is 2-4 kebab-case words from the issue title. Never branch from
`main`, never work directly on `dev`.

### 5. Implement

- Read the issue body fully: acceptance criteria, blockers, linked docs.
- Discovery first: read the files and `.claude/rules/` entries that govern
  the area before editing anything.
- Write tests with the code, not after it. A change without a test needs a
  stated reason in the PR body.
- Run `make check` after each meaningful step, not only at the end.
- Stay inside the issue scope. File a new issue for adjacent problems you
  find; do not fix them silently.

### 6. Verify

- `make check` MUST pass locally before pushing, because CI runs the same
  gates and a red push wastes a round trip.
- Attempt cap: three fix cycles on the same failure, then Escalate.
- Never `--no-verify`. Never weaken a test or rule to pass.

### 7. Pull request

    git push -u origin feat/<N>-<slug>
    gh pr create --base dev --title "<type>(<scope>): <subject>" --body "<what changed and why>

    Fixes #<N>"
    gh pr checks --watch

- The title MUST be a Conventional Commit, because it becomes the squash
  commit on `dev`.
- Keep the PR green: fix CI failures immediately, same three-attempt cap.
- Do not merge your own PR. Merging is a separate review step.

### 8. Record lessons

When the task surfaced a non-obvious fact (a game type behaves unexpectedly,
a tool needs a flag, a Windows trap), append one dated line to
`.claude/lessons-learned.md` inside the same PR. Skip when there is nothing
new; an empty entry is noise.

### 9. Report

End with exactly this summary, then stop. One issue per invocation.

    ## Work report
    - Issue: #<N> <title>
    - Branch: feat/<N>-<slug>
    - PR: <url> (CI: green | red)
    - Gates: make check <pass|fail>; CI <pass|fail>
    - Lessons recorded: <yes: one line | no>
    - Follow-ups filed: <#s | none>
    - Next selectable issue: #<N> (not started)

## Escalate

After three failed attempts on the same gate or CI failure:

1. Push the branch as-is, because finished-but-uncommitted work is the worst
   possible end state.
2. Comment on the issue: what was tried, the exact failing output, the
   suspected cause.
3. Add the `blocked` label to the issue.
4. Report and stop.

## Anti-patterns

- Grading your own homework: reporting done without a green gate run.
- Relaunching without the Orient step.
- Batch mode: touching more than one issue in a single invocation.
- Silent scope creep, silent test weakening, silent hook edits.
- Merging your own PR.
