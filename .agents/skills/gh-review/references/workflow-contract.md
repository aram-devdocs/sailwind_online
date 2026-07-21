# Workflow contract: gh-review

The machine-checkable shape of this skill. The validator confirms the files and
sections named here exist; this document states the behavior they encode.

## Inputs

- One review target: a PR number (`gh pr checkout <N>`) or the local diff
  (`git diff dev...HEAD`).
- Read access to the CI scripts this mirrors under `scripts/` and the workflow
  at `.github/workflows/ci.yml`.

## Outputs

- A bucketed review of the changed files.
- Exactly one verdict line whose first token is APPROVE, REQUEST-CHANGES, or
  REJECT.

## Invariants

- The review calls the same scripts CI calls; it does not reimplement a gate,
  because a reimplemented gate drifts from the one CI trusts.
- A red `make validate` forces at least REQUEST-CHANGES.
- Any blocker-class finding forces at least REQUEST-CHANGES.
- The review ends with exactly one verdict line, never zero and never two.

## Mirrored gates

| Gate            | Command                                                 |
|-----------------|---------------------------------------------------------|
| Canonical       | make validate                                           |
| Leak scan       | bash scripts/check-no-leak.sh                           |
| Game-IP guard   | bash scripts/guard-no-game-ip.sh                        |
| Architecture DAG| dotnet test tests/Sailwind.Architecture.Tests           |
| Rust checks     | cargo test --workspace                                  |

## Blocker-class findings

Each forces at least REQUEST-CHANGES and MUST be named as `path:line`:

- a suppressed or downgraded warning,
- a hand-edited generated file,
- a game reference outside `apps/**` and the api-adapters package,
- a leak term (a private organization, client, product, repository, or person
  name).

## File buckets

contracts, api, server, client, infra, docs. Every changed path lands in exactly
one bucket and is reviewed for that bucket's failure mode.
