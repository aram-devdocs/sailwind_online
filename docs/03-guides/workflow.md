# Workflow

Work is organized as GitHub issues on a board, driven through short-lived
branches into pull requests. The issue list is the backlog; there are no plan or
status files in the repo, because GitHub holds that state. The only durable
in-repo state is the run bookkeeping under `.agents/runs/` and the
`.agents/lessons-learned.md` whiteboard.

## Branch model

There are two long-lived branches. `dev` is the integration branch and the
default; every feature branch starts from it and merges back into it. `main` is
release-only and receives changes only through a pull request from `dev`. There
is no `master` anywhere.

You branch `feat/<issue-number>-<slug>` from `dev`, because branch protection
blocks direct pushes and a per-issue branch keeps each change reviewable on its
own.

## Pull requests

- PR titles are Conventional Commits (`type(scope): subject`), because the
  squash-merge title becomes the commit that release tooling reads. Types are
  feat, fix, refactor, docs, test, chore; scopes are api, client, server,
  contracts, infra, docs, release.
- `make validate` MUST pass locally before you push, because it mirrors CI
  exactly and a red push wastes a round trip.
- `/work` MAY merge the PR it created only through its gated merge script,
  because that script rechecks the completed run, all independent review
  verdicts, the current PR head, and required CI checks before merging.
- A failed merge precondition MUST stop without merging, because an unchecked
  self-merge would bypass the review boundary.
- Merges are squash-only, so `dev` history reads as one Conventional Commit per
  change.

## The canonical gate

`make validate` is the one gate, and it mirrors CI so that green locally means
green in CI. It runs the instruction-layer convention check, the leak scan, the
game-IP guard, the governance-hook fixture suite, the warnings-as-errors
game-free build, every game-free test suite (contracts, net, sync, and the
architecture DAG), and the Rust format, lint, and test steps. Where the game
assemblies are present it also builds the full solution and runs the
game-coupled surface tests.

## Releases

A release is a pull request from `dev` to `main`. `main` never takes a direct
push. The Conventional Commit titles accumulated on `dev` are what release
tooling reads to produce the changelog and version.

## The self-driving loop: `/work`

The `/work` skill drives one issue from open through a gated squash merge into
`dev` without human steering, and it is resumable: it reads its state from disk,
not memory, so a run that died mid-task recovers cleanly. The full loop is in
`.agents/skills/work/SKILL.md`. One issue per invocation.

1. Orient and resume. It fetches, inspects the working tree, and reads the
   active run under `.agents/runs/`. Each run keeps durable state in
   `.agents/runs/<run>/state.json`; if a run is active it reconciles the
   recorded phase against reality and resumes rather than starting fresh.
2. Select. With no active run, it picks the highest-priority unblocked issue:
   lowest milestone first, then priority label (P0, P1, P2), then lowest issue
   number. An issue is skipped when it is labeled blocked, when its body names
   an open blocker, when it was freshly claimed, or when an open PR already
   references it.
3. Implement through the roster. The orchestrator never edits files itself; it
   dispatches the subagents below and drives the per-issue lifecycle in
   `.agents/skills/gh-issue`.
4. Review, verify, open the PR. It runs the fixed review gates in order, makes
   `make validate` pass, opens the PR, and keeps it green. Before the four
   reviewers run, the state machine records the clean commit in
   `reviewed-head` and clears old verdicts. Any later commit requires all four
   reviews again.
5. Recheck and merge. After `/gh-issue` reaches `done`, `/work` reads that
   completed run and requires `plan_open=0` plus four `APPROVE` verdicts. Its
   deterministic merge script derives owner/repository only from the immutable
   canonical `issue_url` recorded at run creation and checks it against the
   strictly parsed `.agents/repository.json` bootstrap identity. It never
   discovers identity from checkout remotes. Using that explicit repository,
   it verifies the recorded PR is open, not a draft, targets `dev`, comes from
   the recorded branch, links the recorded issue, reports a clean merge state,
   and has no pending or failed required checks.
   It rereads the head before a squash merge guarded by that exact commit,
   rechecks linkage and required checks at the final mutation boundary,
   omits GitHub's unguarded branch-delete flag, then confirms `MERGED`, `CLOSED`,
   and independent absence of the exact recorded remote feature ref. Branch
   cleanup uses a SHA-bound lease, so a concurrent move cannot be deleted.
   Cleanup retains the active-run marker until those confirmations succeed, so
   a restart resumes this handoff before selecting another issue.

Supporting skills, all under `.agents/skills/`: `gh-runbook` (decompose a large
issue), `gh-issue` (the per-issue lifecycle state machine), `gh-review` (the
local mirror of the review gates), `subagent-driven-development` (the
delegate-never-implement model), and `session-continuity` (resume across a
compaction).

## The subagent roster

Implementation and review are split across seven subagents, defined under
`.claude/agents/`. The orchestrator composes them; it does not do their work.

- `01-implementer` writes the code and tests for the issue.
- `02-spec-reviewer` checks the change against the issue's stated intent.
- `03-code-quality-reviewer` checks readability, structure, and the no-legacy
  rules.
- `04-architecture-validator` checks the change against the dependency DAG and
  layer discipline.
- `05-security-auditor` checks for leaks, unsafe patterns, and IP exposure.
- `06-test-runner` runs the suites and reports failures.
- `07-debugger` investigates a failure the runner surfaced.

## The review gates

The four review gates run in a fixed order after implementation, and the order
is load-bearing: spec, then quality, then architecture, then security
(`02` to `03` to `04` to `05`). Spec comes first because a change that does not
match its issue is not worth reviewing further; security comes last because it
audits the final, settled diff. Each verdict is recorded through the run state
machine, and a request-changes verdict MUST be fixed and its gate re-run before
the PR opens, because an unaddressed gate defeats the point of gating.

## Governance hooks

The run is fenced by governance hooks under `.claude/hooks/`, wired in
`.claude/settings.json`. They enforce the mechanics the prose above only
describes: they block a `--no-verify` push, scan writes for secrets, keep the
review gates in order, and save session state before a compaction. The hooks
stay inert unless a `/work` run is active, so an ordinary editing session runs
unfenced.

## How merging works in practice

The `/gh-issue` state machine remains responsible for producing a green PR and
does not merge. It records the selected canonical GitHub issue URL as immutable
`issue_url`; legacy runs missing the marker use the locked `migrate-issue-url`
command with an independently recorded URL and otherwise fail closed. Active
non-`done` migration additionally requires the exact clean recorded worktree
and branch. `/work` owns the post-CI merge and
MUST call `.agents/skills/work/scripts/gated_merge.py`, because a single
deterministic path prevents a conversational shortcut around the gates.

The script accepts only a completed run with no open plan items and four
`APPROVE` review verdicts. It also requires the current PR head to equal the
state-machine-owned `reviewed-head`, so a CI-fix commit cannot inherit verdicts
for an older diff. The required CI `gate` is tied to that same PR head and
mirrors `make validate`; the script reads the head again immediately before
merging and gives it to GitHub as the expected head commit. GitHub refuses the
merge if the branch changed in that interval. A successful run squash-merges,
deletes the remote feature branch, confirms the PR is `MERGED`, and confirms
the linked issue is `CLOSED`.

The confirmation path uses bounded issue-closure polling and fixed timeouts for
every external command. If confirmation fails after GitHub accepted the merge,
the active run remains discoverable. A retry verifies the exact already-merged
PR, reviewed head, required checks, and closing issue link, skips a second merge
call, and finishes confirmation before clearing the active marker. If the
recorded remote branch remains at the reviewed head, the retry deletes it with
an atomic `--force-with-lease` bound to that commit and rechecks it. The push
targets the HTTPS URL derived from durable `issue_url`, not a configurable
local remote or an implicit `gh repo view`. A move before or during
deletion fails the lease and is never deleted.

The final active-marker clear goes through the run state machine under its
global lock and deletes only a marker still naming the completed run. A marker
replaced by another run is preserved.

Worktree cleanup reads the full Git worktree registry entry even when the
recorded directory is missing. The path, branch, and head must match the run's
recorded branch and reviewed commit. An existing worktree must still be clean,
including untracked files, immediately before normal removal. Force is limited
to removing the exact matched registry entry after its path is verified absent.
A dirty path or replacement registration fails without mutation. The state
machine holds an ownership-safe advisory lifecycle lock through the final
registry/path absence check and `done` write, while `init-run` holds the same
lock for creation. Process exit releases a crashed owner's lock; file age never
steals a live lock. A removal, identity, or verification error keeps the prior
phase and active marker so cleanup can be retried.

Before a run exists, `/work` reads the one-field tracked
`.agents/repository.json` file and passes its
`aram-devdocs/sailwind_online` value to issue selection, blocker lookup,
assignment, and comment commands with `--repo`. The selected explicit result
supplies the canonical URL to `init-run`. Missing, malformed, extra-field, or
mismatched configuration stops the run before GitHub mutation.

Any mismatch prints the exact failure and stops. `/work` does not merge a
different PR, repair state by hand, or select another issue in that invocation.
