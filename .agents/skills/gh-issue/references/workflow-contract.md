# gh-issue workflow contract

The exact, load-bearing contract between the `/gh-issue` state machine
(`scripts/gh_issue_run.py`), the run state on disk, and the governance hooks in
`.claude/hooks`. Change nothing here without changing the readers to match.

## state.json flat-key schema

The state file is `.agents/runs/<run_id>/state.json`, written only by
`gh_issue_run.py` with `json.dump(indent=2)` so every key sits on its own line.

The hooks read this file with a grep-based fallback (see
`.claude/hooks/_lib.sh`, `json_str` / `json_num`), NOT a JSON parser. Therefore
every value MUST be a flat top-level string. Nested arrays and objects are
FORBIDDEN, because the grep reader cannot descend into them and would read
garbage. `update-state` rejects any key outside the set below and refuses a
non-string value.

| key                 | meaning                                                     | example                    |
| ------------------- | ----------------------------------------------------------- | -------------------------- |
| `run_id`            | run identifier and directory name, `<N>-<slug>`             | `42-fix-login`             |
| `issue`             | GitHub issue number, as a string                            | `42`                       |
| `phase`             | current lifecycle phase (see table)                         | `implement`                |
| `branch`            | working branch, `feat/<N>-<slug>`                           | `feat/42-fix-login`        |
| `worktree`          | worktree path, `.worktrees/<N>-<slug>`                      | `.worktrees/42-fix-login`  |
| `pr`                | PR number or URL; empty until the PR exists                 | `137`                      |
| `gate_spec`         | spec-review verdict; empty until recorded                   | `APPROVE`                  |
| `gate_quality`      | quality-review verdict; empty until recorded                | `APPROVE`                  |
| `gate_architecture` | architecture-review verdict; empty until recorded           | `REQUEST-CHANGES`          |
| `gate_security`     | security-review verdict; empty until recorded               | `APPROVE`                  |
| `plan_open`         | count of open plan items; `"0"` or `""` means none open     | `3`                        |
| `updated_at`        | UTC ISO-8601 timestamp of the last write                    | `2026-07-21T14:03:11Z`     |

A freshly initialized run has `phase=investigate`, empty gates, empty `pr`,
empty `plan_open`, and `branch`/`worktree` derived from `run_id`.

### Reviewed-head companion

The flat schema stays unchanged. The state machine writes the reviewed commit
to `.agents/runs/<run_id>/reviewed-head` immediately before the fixed review
sequence:

    python .agents/skills/gh-issue/scripts/gh_issue_run.py record-reviewed-head

The command requires the recorded worktree to be clean, on the recorded branch,
and in `phase=review`. Under the run's single-writer lock it clears all four
`gate_*` verdicts, updates `state.json`, and atomically writes the 40-character
commit marker. Clearing verdicts before writing the marker is fail-closed:
interruption can leave gates empty, but cannot make old approvals apply to a
new commit.

Any commit after this command requires recording the new head and rerunning all
four reviewers. `/work` refuses to merge when the marker is missing, malformed,
or different from the PR head.

### Load-bearing write invariants

- **Backup before write.** `state.json` is copied to `state.json.bak` before
  any modification, so a crashed write leaves a recoverable prior state. The
  new state is written to `state.json.tmp` and atomically renamed into place.
- **Single-writer lock.** Every write holds `.lock` in the run directory,
  created with `O_CREAT|O_EXCL`. A lock older than 30 seconds is treated as
  abandoned and reclaimed, so a dead writer cannot wedge the run.

## Phase transition table

`phase` is one of: `investigate plan implement verify review pr wait-ci
cleanup done`. Transitions are linear; each has an exit condition recorded
through the state machine.

| from          | to            | exit condition (what must be true to advance)                     |
| ------------- | ------------- | ----------------------------------------------------------------- |
| investigate   | plan          | issue and governing rules read; change scoped                     |
| plan          | implement     | plan written; `plan_open` set to the item count                   |
| implement     | verify        | implementer done; `plan_open` drawn down to `0`                   |
| verify        | review        | `make validate` passes locally                                    |
| review        | pr            | `reviewed-head` recorded; all four `gate_*` verdicts recorded for that commit, none blocking |
| pr            | wait-ci       | PR opened to `dev`; `pr` recorded                                 |
| wait-ci       | cleanup       | CI green (`poll-pr` reports PASS)                                 |
| cleanup       | done          | worktree removed; active marker retained for `/work` merge resume |
| done          | (terminal)    | `/gh-issue` stops; `/work` owns merge, confirmation, and active-marker clearing |

The `review` phase runs the four gates in the fixed order spec -> quality ->
architecture -> security. A REJECT or an unaddressed REQUEST-CHANGES blocks the
advance; the implementer is re-dispatched and the gate re-run.

## Hook interplay

Which hook reads which key. All hooks are inert unless a run is active (that is,
`.agents/runs/active` names a run whose `state.json` exists, or some run's
`phase` is not `done`). Cleanup intentionally retains the active marker at
`phase=done`, so a crash before or after merge stays discoverable. `/work`
clears that marker only after it confirms both the merged PR and closed issue.

| hook                          | trigger        | reads                          | effect                                                                 |
| ----------------------------- | -------------- | ------------------------------ | ---------------------------------------------------------------------- |
| `completion-check.sh`         | Stop           | `gate_spec` `gate_quality` `gate_architecture` `gate_security` | blocks the first stop while any gate is empty; second stop overrides   |
| `plan-completion-guard.sh`    | Stop           | `plan_open`                    | blocks the first stop while `plan_open > 0`; second stop overrides     |
| `review-gate-tracker.sh`      | PostToolUse (Agent/Task) | subagent type + response verdict | WRITES `gate_<name>` back into state via sed after a gate subagent finishes |
| `context-loader.sh`           | SessionStart   | active marker / `phase`        | prints a reminder; names the active run's state file                   |
| `state-cleanup.sh`            | SessionEnd     | (none)                         | clears the per-session one-time-block flags                            |

Because `review-gate-tracker` writes `gate_<name>` with a `sed` line
substitution, those keys MUST stay simple `"key": "value"` lines. Keeping every
value a flat string is what makes both the grep readers and the sed writer
work.

## The runs-dir override

`gh_issue_run.py` resolves its runs directory as `--runs-dir` argument, then
`$SW_RUNS_DIR`, then `<repo-root>/.agents/runs`. Tests and dry exercises pass
`--runs-dir` (and `--no-git` on `init-run`/`cleanup-worktree`) so they exercise
the machine without creating a real run or worktree. The same `SW_RUNS_DIR`
seam is honored by the hooks (`.claude/hooks/_lib.sh`).
