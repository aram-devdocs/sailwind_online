---
name: gh-issue
description: |
  Drive one GitHub issue through the full self-driving lifecycle in an isolated
  worktree: investigate, plan, implement, verify, review, PR, wait for CI, and
  clean up. Every phase is gated and every state transition goes through the
  gh_issue_run.py state machine, never a hand edit. Dispatches the implementer
  and the four review subagents; does not merge its own PR.
user-invocable: true
---

# gh-issue

## Purpose

Take a single, already-selected GitHub issue from open to a green pull request
against `dev`, running the whole lifecycle as a durable state machine so a
crashed session can resume from disk rather than from memory. This skill is the
self-driving core: `/work` selects the issue and hands it here.

The run's truth lives in `.agents/runs/<N>-<slug>/state.json`. That file is
written ONLY by `scripts/gh_issue_run.py`. Reference
`.agents/rules/subagent-workflow.md` for the roster and the fixed review order;
this skill drives that roster through the phases below.

## Phase lifecycle

The run walks these phases in order. Each is gated: it MUST satisfy its exit
condition, recorded through the state machine, before the next begins.

    investigate -> plan -> implement -> verify -> review -> pr -> wait-ci -> cleanup -> done

- **investigate** Read the issue body in full (acceptance criteria, blockers,
  linked docs), then the files and `.agents/rules/` entries that govern the
  area. Discovery first, because editing before reading is how scope creep and
  layer violations start. Exit: the change is understood and scoped.
- **plan** Write the work as an explicit, checkable plan. Record the open-item
  count with `update-state --key plan_open`. Exit: the plan exists and
  `plan_open` reflects it.
- **implement** Dispatch the `01-implementer` subagent to write code and tests
  together (test first, because a test written after the code only confirms
  what was built). The orchestrator MUST NOT edit files itself. As plan items
  close, lower `plan_open`. Exit: all plan items done, `plan_open` is `0`.
- **verify** Run `make validate` (the canonical gate). Attempt cap: three fix
  cycles on the same failure, then escalate. Never `--no-verify`, never weaken
  a test to go green. Exit: `make validate` passes locally.
- **review** Record the reviewed head, then run the four review gates in fixed
  order (see below). Exit: all four verdicts apply to the recorded commit and
  none is REJECT / unaddressed REQUEST-CHANGES.
- **pr** Push the branch and open a PR to `dev` with a Conventional Commit
  title and `Fixes #<N>` in the body. Record the PR with
  `update-state --key pr`. Exit: PR exists and is recorded.
- **wait-ci** Poll checks with `gh_issue_run.py poll-pr`. Fix red CI, same
  three-attempt cap. Exit: CI is green.
- **cleanup** `gh_issue_run.py cleanup-worktree` removes the worktree, sets
  `phase=done`, and retains the active marker for `/work`. Exit: worktree gone,
  phase done, merge handoff remains discoverable. If worktree removal fails,
  stop with the prior phase and active marker intact, because a Windows file
  lock must remain retryable rather than becoming a false `done`.
- **done** Terminal for `/gh-issue`. The run reports and stops. It does NOT
  merge its own PR or clear the handoff that `/work` must resume.

## State transitions

Every transition goes THROUGH `scripts/gh_issue_run.py`. Never hand-edit
`state.json`: the governance hooks read it with a grep-based fallback that
depends on the exact flat-key shape, and a hand edit silently breaks them.

    # start a run (also creates the worktree)
    python .agents/skills/gh-issue/scripts/gh_issue_run.py init-run --issue 42 --slug fix-login

    # advance a phase
    python .agents/skills/gh-issue/scripts/gh_issue_run.py update-state --key phase --value implement

    # record the plan size, then draw it down
    python .agents/skills/gh-issue/scripts/gh_issue_run.py update-state --key plan_open --value 5

    # record the PR once opened
    python .agents/skills/gh-issue/scripts/gh_issue_run.py update-state --key pr --value 137

    # read state on resume
    python .agents/skills/gh-issue/scripts/gh_issue_run.py get-state

The exact flat-key schema, the phase transition table, and which hook reads
which key live in `references/workflow-contract.md`.

## Worktree isolation

Each issue runs in its own git worktree at `.worktrees/<N>-<slug>` on branch
`feat/<N>-<slug>`, created by `init-run`. Isolation lets one run proceed
without disturbing the main checkout or another run.

One trap: `lib/` is gitignored, so a git worktree starts WITHOUT it.
Game-coupled builds (anything touching `packages/Sailwind.Api.Adapters` or the
game-coupled tests) need `lib/` present in the worktree. Options:

- Do game-coupled work in the main checkout, not the worktree, or
- Copy or symlink `lib/` into the worktree before a game-coupled build.

Game-free work (the `packages/` suites, `cargo`, contracts) runs in the
worktree cleanly and is the default.

## Review gates

The `review` phase dispatches the four reviewers in the fixed order, each
read-only:

    02-spec-reviewer -> 03-code-quality-reviewer -> 04-architecture-validator -> 05-security-auditor

Before dispatching `02-spec-reviewer`, bind the clean worktree commit and clear
every prior verdict:

    python .agents/skills/gh-issue/scripts/gh_issue_run.py record-reviewed-head

This command MUST run through the state machine, because the `reviewed-head`
companion marker is the durable proof that all four verdicts apply to one exact
commit. Any commit after the marker is recorded invalidates the review set:
return to `review`, record the new head, and run all four reviewers again.

Each emits a verdict line beginning APPROVE, REQUEST-CHANGES, or REJECT. The
`review-gate-tracker` hook records each verdict into `state.json` as
`gate_spec`, `gate_quality`, `gate_architecture`, `gate_security` so the
`completion-check` hook can see the run is unfinished while any gate is empty.
Do not overwrite a reviewer's verdict; address REQUEST-CHANGES by dispatching
the implementer again, then re-running the gate.

Security review MUST NOT be parallelized with the others, because it needs the
whole change in view.

## Resume

On re-entry, read state, not memory. `/work` checks `.agents/runs/active` and
runs `validate-resume`, which reconciles the recorded phase against reality
(worktree present and clean/dirty, branch exists, PR state, outstanding gates)
and prints an action list. Resume from the earliest phase whose reality is
incomplete. Never trust the recorded phase at face value: a phase can say
`implement` while the worktree is already complete but uncommitted, the classic
dead-run failure.

When the active run is already at `done`, its missing worktree is expected.
`validate-resume` routes directly to `/work`'s gated merge. The active marker
MUST remain until merge and issue-closure confirmation succeed, because clearing
it earlier would let issue selection skip an unfinished handoff.

## Boundaries

- The orchestrator delegates; it does not implement (dispatch `01-implementer`).
- Reviewers are read-only; never let a reviewer edit the work it judges.
- The `/gh-issue` run does NOT merge its own PR. The top-level `/work` wrapper
  owns the post-CI merge and may proceed only after its separate merge gate
  proves the PR head equals this run's `reviewed-head`.
- One issue per run. File a new issue for adjacent problems; do not fix them
  silently.
