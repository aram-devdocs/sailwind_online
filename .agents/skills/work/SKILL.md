---
name: work
version: 2.1.0
description: |
  The top-level self-driving loop. Orients against repo and run state, resumes
  an active run or selects the highest-priority unblocked GitHub issue, routes
  large issues through /gh-runbook and the rest straight to /gh-issue, drives
  the issue to a green PR against dev through gated subagent phases, mirrors the
  review locally with /gh-review, records lessons, and squash-merges only after
  rechecking the completed run and current GitHub state. One issue per
  invocation.
user-invocable: true
---

# Work

## Purpose

Drive one GitHub issue from open through a gated squash merge into `dev` without
human steering, as a durable, resumable, subagent-driven run. The issue list is
the backlog; this skill is the executor. The heavy lifting lives in
`/gh-issue`; this loop selects the work, resumes crashed runs, performs the
post-CI merge, and enforces the one-issue-per-invocation discipline.

Reference: `/gh-runbook` (decomposing a large issue), `/gh-issue` (the
per-issue lifecycle), `/gh-review` (the local review mirror), and
`.agents/rules/subagent-workflow.md` plus the `subagent-driven-development`
guidance (the orchestrator delegates, never implements).

## Preconditions

Stop and report instead of proceeding when any of these fail:

- `gh auth status` shows the aram-devdocs account.
- `AGENTS.md` and the applicable `.agents/rules/` have been read this session.
- The orient step below has reconciled any prior run.

## Process

### 1. Orient and resume (re-entry first, always)

Never assume a fresh world. A previous run may have died mid-task; resume reads
disk, not memory. In order:

    git fetch origin
    git status --porcelain

Then check for an active run:

    cat .agents/runs/active 2>/dev/null

If a run is active, do NOT start a new one. Reconcile it and resume:

    python .agents/skills/gh-issue/scripts/gh_issue_run.py validate-resume

`validate-resume` reconciles the recorded phase against reality (worktree
present and clean or dirty, branch exists, PR state, outstanding gates) and
prints an action list. Hand the active run back to `/gh-issue`, which resumes
from the earliest phase whose reality is incomplete. A complete-but-uncommitted
worktree is the classic dead-run trap: inspect the diff, then finish or discard
deliberately, never blind-reset. When the resumed run reaches `done`, continue
to the gated merge in step 8, report, and stop; do not also pick a new issue
this invocation.

If the active run is already at `done`, go directly to step 8. Cleanup retains
the active marker for this handoff, so a crash cannot make an unmerged PR
disappear from issue selection. The gated merge clears the marker only after it
confirms both the PR merge and issue closure.

If no run is active and the tree is clean:

    git switch dev && git pull --ff-only

### 2. Select an issue

Only when no run is active.

    gh issue list --state open --limit 100 --json number,title,labels,milestone,assignees

Selection order, applied in sequence:

1. Lowest milestone (M0 before M1, and so on). No milestone sorts last.
2. Priority label: P0, then P1, then P2.
3. Lowest issue number.

Skip an issue when any of these hold:

- It carries the `blocked` label.
- Its body says `Blocked by #N` and issue N is still open
  (`gh issue view N --json state`).
- It is assigned and shows activity newer than 24 hours (claimed).
- An open PR already references it.

If nothing is selectable, report that and stop.

### 3. Claim

    gh issue edit <N> --add-assignee @me
    gh issue comment <N> --body "Picking this up. Branch: feat/<N>-<slug>"

`<slug>` is 2-4 kebab-case words from the issue title.

### 4. Route

Read the issue body fully first, then choose the path:

- **Large issue** (multiple deliverables, needs decomposition into ordered
  sub-issues or a phased plan): delegate to `/gh-runbook`. It breaks the work
  down and files or sequences the pieces; this loop then drives the first
  actionable piece through `/gh-issue`.
- **Right-sized issue** (a single coherent change): go straight to `/gh-issue`.

### 5. Drive /gh-issue to a green PR

    python .agents/skills/gh-issue/scripts/gh_issue_run.py init-run --issue <N> --slug <slug>

Then run the `/gh-issue` lifecycle: investigate -> plan -> implement -> verify
-> review -> pr -> wait-ci -> cleanup -> done. Every transition goes through
`gh_issue_run.py`; the `implement` phase dispatches `01-implementer` (the
orchestrator never edits files); the `review` phase runs `02` -> `03` -> `04`
-> `05` in the fixed order with each verdict recorded through the state machine.
Immediately before `02`, run `gh_issue_run.py record-reviewed-head`; it binds
the clean worktree commit and clears every old verdict. Any later commit,
including a CI fix, MUST return to `review`, record the new head, and rerun all
four reviewers, because verdicts for an earlier commit cannot authorize the
merge.
`make validate` is the canonical gate and MUST pass locally before the PR.
Attempt cap: three fix cycles on the same failure, then Escalate.

### 6. Local review mirror

Before opening or finishing the PR, run `/gh-review` as the local mirror of the
gate order, so the four reviewers see the whole change together and a
REQUEST-CHANGES is fixed before CI (and before a human) ever sees it. Feed any
REQUEST-CHANGES back into the `implement` phase, then re-run the gate.

### 7. Record lessons

When the task surfaced a non-obvious fact (a game type behaves unexpectedly, a
tool needs a flag, a Windows trap), append one dated line to
`.agents/lessons-learned.md` inside the same PR. Skip when there is nothing new;
an empty entry is noise.

### 8. Gated squash merge

`/gh-issue` ends at a green PR and keeps its state schema unchanged. After its
run reaches `done`, `/work` owns the merge:

    python .agents/skills/work/scripts/gated_merge.py --run-id <run_id>

The script MUST be the only merge path in this loop, because it binds the
operation to the completed run rather than trusting conversational memory. It
requires `phase=done`, `plan_open=0`, all four recorded verdicts equal to
`APPROVE`, a state-machine-owned `reviewed-head`, and an issue, branch, and PR
that exactly match the run. It then rechecks through `gh` that the PR is open,
not a draft, targets `dev`, comes from the recorded branch, links the recorded
issue for closure, reports `mergeStateStatus=CLEAN`, and has no pending or
failed required checks. The PR head MUST equal `reviewed-head`, because every
review verdict must cover the exact commit merged. The required `gate` check
proves that same PR head passed the CI mirror of `make validate`.

Immediately before merging, the script reads the PR again and refuses a changed
head, rechecks issue linkage, and reads required checks again. It passes that
exact head to `gh pr merge --match-head-commit` only when the final check read
still passes, uses squash merge, and requests remote branch deletion. It then
confirms the PR is `MERGED` and polls the linked issue for bounded closure
confirmation. Every `gh` call has a fixed timeout and fails closed with the
command purpose.

The merge command is retry-safe. If GitHub accepted the squash merge but a
confirmation call failed or issue closure was delayed, rerun the same command.
It accepts only the exact recorded PR, branch, issue link, reviewed head, and
required checks; when that PR is already `MERGED`, it skips the merge call,
finishes bounded confirmation, and clears the active handoff.

If the command prints `STOP`, report its exact precondition failure and stop
without merging or selecting another issue. If it prints `ERROR`, report the
exact merge or confirmation failure and stop without claiming success. Use
`--dry-run` only for diagnostics, because it checks every precondition but does
not merge.

### 9. Report and stop

End with exactly this summary, then stop. One issue per invocation.

    ## Work report
    - Issue: #<N> <title>
    - Run: <N>-<slug> (phase: <final phase>)
    - Branch: feat/<N>-<slug>
    - PR: <url> (MERGED | not merged: <exact failure>)
    - Issue state: CLOSED | <actual state>
    - Gates: spec <v>; quality <v>; architecture <v>; security <v>
    - make validate: <pass | fail>
    - Lessons recorded: <yes: one line | no>
    - Follow-ups filed: <#s | none>
    - Next selectable issue: #<N> (not started)

## Escalate

After three failed attempts on the same gate or CI failure:

1. Record the run's state through `gh_issue_run.py` and push the branch as-is,
   because finished-but-uncommitted work is the worst possible end state.
2. Comment on the issue: what was tried, the exact failing output, the
   suspected cause.
3. Add the `blocked` label to the issue.
4. Report and stop.

## Anti-patterns

- Grading your own homework: reporting done without a green gate run and
  recorded verdicts.
- Relaunching without the orient/resume step, or starting a new issue while a
  run is active.
- Batch mode: touching more than one issue in a single invocation.
- Implementing in the orchestrator instead of dispatching `01-implementer`.
- Hand-editing `state.json` instead of going through `gh_issue_run.py`.
- Silent scope creep, silent test weakening, silent hook edits.
- Calling `gh pr merge` directly instead of the gated merge script.
- Clearing a completed run's active marker before merge confirmation.
- Starting the next issue after either a successful or failed merge attempt.
