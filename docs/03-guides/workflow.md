# Workflow

Work is organized as GitHub issues on a board, driven through short-lived
branches into pull requests. The issue list is the backlog; there are no plan or
status files in the repo, because GitHub holds that state.

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
- The fast gate MUST pass locally before you push, because CI runs the same
  scripts and a red push wastes a round trip.
- You do not merge your own PR. Merging is a separate review step, so nothing
  lands on `dev` by the same hand that wrote it.
- Merges are squash-only, so `dev` history reads as one Conventional Commit per
  change.

## Releases

A release is a pull request from `dev` to `main`. `main` never takes a direct
push. The Conventional Commit titles accumulated on `dev` are what release
tooling reads to produce the changelog and version.

## Running the loop with `/work`

The `/work` skill drives one issue from open to a green PR against `dev` without
human steering. It orients against the current repo and board state (so a run
that died mid-task resumes cleanly), picks the highest-priority unblocked issue,
branches, implements with tests, opens the PR, records any lesson learned, and
reports. One issue per invocation. The full loop is in
`.agents/skills/work/SKILL.md`.

Selection order is lowest milestone first, then priority label (P0, P1, P2),
then lowest issue number. An issue is skipped when it is labeled blocked, when
its body names an open blocker, when it is freshly assigned, or when an open PR
already references it.

## How merging works in practice

A person (or, later, a review workflow) reviews the green PR and merges it. A
single-maintainer account cannot approve its own PR, so the gate that protects
`dev` is the CI status check, not a required approval, and the no-self-merge
rule keeps the review boundary intact.
