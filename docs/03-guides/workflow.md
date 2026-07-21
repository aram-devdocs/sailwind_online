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
- You do not merge your own PR. Merging is a separate review step, so nothing
  lands on `dev` by the same hand that wrote it.
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

The `/work` skill drives one issue from open to a green PR against `dev` without
human steering, and it is resumable: it reads its state from disk, not memory,
so a run that died mid-task recovers cleanly. The full loop is in
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
   `make validate` pass, opens the PR, and keeps it green. It never merges its
   own PR.

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

A person (or, later, a review workflow) reviews the green PR and merges it. A
single-maintainer account cannot approve its own PR, so the gate that protects
`dev` is the CI status check, not a required approval, and the no-self-merge
rule keeps the review boundary intact.
