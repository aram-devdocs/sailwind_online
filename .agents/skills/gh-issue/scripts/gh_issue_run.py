#!/usr/bin/env python3
"""Durable run-state machine for the /gh-issue self-driving lifecycle.

This script is the ONLY writer of a run's state.json. Every phase transition
and every gate verdict flows through here so the governance hooks
(.claude/hooks) can read a stable, machine-parseable file. Never hand-edit
state.json.

Flat-key contract (load-bearing)
--------------------------------
The .claude/hooks read state.json with a grep-based fallback, not a JSON
parser. Every key MUST therefore be a FLAT top-level string, one "key": "value"
line per key. Nested arrays and objects are FORBIDDEN because the grep reader
cannot descend into them. state.json is written with json.dump(indent=2) so
each key lands on its own line.

Allowed flat keys (nothing else may be set):
    run_id            <N>-<slug> identifier, also the run directory name
    issue             the GitHub issue number, as a string
    phase             one of: investigate plan implement verify review
                              pr wait-ci cleanup done
    branch            feat/<N>-<slug>
    worktree          .worktrees/<N>-<slug>
    pr                the PR number or URL, empty until the PR exists
    gate_spec         spec-review verdict, empty until recorded
    gate_quality      quality-review verdict, empty until recorded
    gate_architecture architecture-review verdict, empty until recorded
    gate_security     security-review verdict, empty until recorded
    plan_open         count of open plan items, as a string ("0"/"" = none)
    updated_at        UTC ISO-8601 timestamp of the last write

Two invariants are load-bearing and MUST NOT be removed:
    1. Backup-before-write: state.json is copied to state.json.bak before any
       write, so a crashed write leaves a recoverable prior state.
    2. Single-writer lock: writes hold .lock (O_CREAT|O_EXCL, with a stale-lock
       timeout) so two processes never interleave a read-modify-write.

Subcommands
-----------
    init-run --issue N --slug SLUG [--resume] [--no-git]
    update-state --key K --value V
    get-state [--key K]
    set-active RUN_ID
    clear-active
    validate-resume
    record-reviewed-head [--run-id RUN_ID]
    poll-pr
    cleanup-worktree [--no-git]

Runs directory resolution (highest precedence first):
    --runs-dir ARG  >  $SW_RUNS_DIR  >  <repo-root>/.agents/runs
The override exists so tests exercise the machine without touching a real run.

Stdlib only: json, argparse, os, subprocess, time, pathlib.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

# The four review gates in fixed order (spec first, security last). Mirrors
# SW_GATE_ORDER in .claude/hooks/_lib.sh; keep the two in sync.
GATE_ORDER = ("spec", "quality", "architecture", "security")

# The phase lifecycle, in order. "done" is terminal.
PHASES = (
    "investigate",
    "plan",
    "implement",
    "verify",
    "review",
    "pr",
    "wait-ci",
    "cleanup",
    "done",
)

# Every flat key the state file may carry. update-state rejects anything else,
# because an unknown key is either a typo or a contract violation the hooks
# cannot read.
ALLOWED_KEYS = (
    "run_id",
    "issue",
    "phase",
    "branch",
    "worktree",
    "pr",
    "gate_spec",
    "gate_quality",
    "gate_architecture",
    "gate_security",
    "plan_open",
    "updated_at",
)

# A .lock older than this many seconds is treated as abandoned by a dead
# process and reclaimed, because a crashed writer must not wedge the run
# forever.
STALE_LOCK_SECONDS = 30

# How long to wait for a live lock before giving up.
LOCK_WAIT_SECONDS = 10

# External probes must not hang a durable run forever.
COMMAND_TIMEOUT_SECONDS = 30


# --------------------------------------------------------------------------- #
# Paths
# --------------------------------------------------------------------------- #

def repo_root():
    """Best-effort repo root: git first, then walk up for .agents, then cwd."""
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, check=False,
        )
        if out.returncode == 0 and out.stdout.strip():
            return Path(out.stdout.strip())
    except (OSError, subprocess.SubprocessError):
        pass
    here = Path.cwd()
    for cand in (here, *here.parents):
        if (cand / ".agents").is_dir():
            return cand
    return here


def runs_dir(args):
    """Resolve the runs directory: --runs-dir > $SW_RUNS_DIR > <root>/.agents/runs."""
    if getattr(args, "runs_dir", None):
        return Path(args.runs_dir)
    env = os.environ.get("SW_RUNS_DIR")
    if env:
        return Path(env)
    return repo_root() / ".agents" / "runs"


def run_dir(args, run_id):
    return runs_dir(args) / run_id


def state_path(args, run_id):
    return run_dir(args, run_id) / "state.json"


def reviewed_head_path(args, run_id):
    return run_dir(args, run_id) / "reviewed-head"


def active_path(args):
    return runs_dir(args) / "active"


# --------------------------------------------------------------------------- #
# Subprocess helpers (git / gh). Never raise; return a tidy triple.
# --------------------------------------------------------------------------- #

def run_cmd(cmd, cwd=None):
    """Run a command, returning (returncode, stdout, stderr). Never raises."""
    try:
        proc = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=True,
            text=True,
            check=False,
            timeout=COMMAND_TIMEOUT_SECONDS,
        )
        return proc.returncode, proc.stdout.strip(), proc.stderr.strip()
    except subprocess.TimeoutExpired:
        return (
            124,
            "",
            f"command timed out after {COMMAND_TIMEOUT_SECONDS} seconds",
        )
    except (OSError, subprocess.SubprocessError) as exc:
        return 127, "", str(exc)


# --------------------------------------------------------------------------- #
# Locking
# --------------------------------------------------------------------------- #

def acquire_lock(rundir):
    """Take the single-writer lock, reclaiming a stale one. Returns lock path."""
    lock = rundir / ".lock"
    rundir.mkdir(parents=True, exist_ok=True)
    deadline = time.time() + LOCK_WAIT_SECONDS
    while True:
        try:
            fd = os.open(str(lock), os.O_CREAT | os.O_EXCL | os.O_WRONLY)
            os.write(fd, f"{os.getpid()} {int(time.time())}\n".encode())
            os.close(fd)
            return lock
        except FileExistsError:
            # Reclaim an abandoned lock left by a dead writer.
            try:
                age = time.time() - lock.stat().st_mtime
            except OSError:
                age = 0
            if age > STALE_LOCK_SECONDS:
                try:
                    lock.unlink()
                except OSError:
                    pass
                continue
            if time.time() > deadline:
                raise SystemExit(
                    f"error: could not acquire {lock} (held for {age:.0f}s); "
                    "another writer is active"
                )
            time.sleep(0.2)


def release_lock(lock):
    try:
        lock.unlink()
    except OSError:
        pass


# --------------------------------------------------------------------------- #
# State read / write
# --------------------------------------------------------------------------- #

def now_utc():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def blank_state(run_id, issue, slug):
    """A fresh state dict with keys in contract order; phase=investigate."""
    return {
        "run_id": run_id,
        "issue": str(issue),
        "phase": "investigate",
        "branch": f"feat/{run_id}",
        "worktree": f".worktrees/{run_id}",
        "pr": "",
        "gate_spec": "",
        "gate_quality": "",
        "gate_architecture": "",
        "gate_security": "",
        "plan_open": "",
        "updated_at": now_utc(),
    }


def read_state(path):
    """Load state.json as a dict. Raises SystemExit with a clear message."""
    if not path.exists():
        raise SystemExit(f"error: no state at {path}")
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise SystemExit(f"error: cannot read {path}: {exc}")
    if not isinstance(data, dict):
        raise SystemExit(f"error: {path} is not a flat object")
    return data


def validate_flat(data):
    """Every value MUST be a string (flat contract). Reject nested structures."""
    for key, val in data.items():
        if not isinstance(val, str):
            raise SystemExit(
                f"error: key '{key}' has non-string value {val!r}; the flat-key "
                "contract forbids nested arrays/objects (the hooks cannot parse "
                "them)"
            )


def write_state(path, data):
    """Backup-then-write under the caller's held lock. Enforces flatness."""
    validate_flat(data)
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.exists():
        backup = path.with_suffix(".json.bak")
        try:
            backup.write_text(path.read_text(encoding="utf-8"), encoding="utf-8")
        except OSError as exc:
            raise SystemExit(f"error: could not back up {path}: {exc}")
    # Write to a temp sibling then replace, so a crash mid-write cannot leave a
    # half-written state.json (the .bak still holds the prior good copy).
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    os.replace(str(tmp), str(path))


# --------------------------------------------------------------------------- #
# Subcommands
# --------------------------------------------------------------------------- #

def cmd_init_run(args):
    run_id = f"{args.issue}-{args.slug}"
    rundir = run_dir(args, run_id)
    spath = state_path(args, run_id)

    if spath.exists() and not args.resume:
        raise SystemExit(
            f"error: run '{run_id}' already exists at {spath}; pass --resume to "
            "reattach to it"
        )

    lock = acquire_lock(rundir)
    try:
        if spath.exists() and args.resume:
            data = read_state(spath)
            data["updated_at"] = now_utc()
            write_state(spath, data)
            print(f"resumed existing run '{run_id}' at phase '{data.get('phase')}'")
        else:
            data = blank_state(run_id, args.issue, args.slug)
            write_state(spath, data)
            print(f"initialized run '{run_id}' (phase=investigate) at {spath}")
    finally:
        release_lock(lock)

    # Mark this run active for the hooks.
    active_path(args).parent.mkdir(parents=True, exist_ok=True)
    active_path(args).write_text(run_id + "\n", encoding="utf-8")
    print(f"active run set to '{run_id}'")

    # Create the isolated worktree. The script does this at /work time; --no-git
    # skips it for tests and for the authoring/dry-exercise path.
    if args.no_git:
        print("no-git: skipped 'git worktree add' (worktree not created)")
        return
    root = repo_root()
    worktree = root / data["worktree"]
    branch = data["branch"]
    if worktree.exists():
        print(f"worktree already present at {worktree}; leaving as-is")
        return
    rc, out, err = run_cmd(
        ["git", "worktree", "add", str(worktree), "-b", branch, "dev"],
        cwd=str(root),
    )
    if rc == 0:
        print(f"created worktree {worktree} on branch {branch}")
    else:
        print(f"warning: 'git worktree add' failed (rc={rc}): {err or out}")


def cmd_update_state(args):
    if args.key not in ALLOWED_KEYS:
        raise SystemExit(
            f"error: '{args.key}' is not an allowed flat key. Allowed: "
            + ", ".join(ALLOWED_KEYS)
        )
    if args.key == "phase" and args.value not in PHASES:
        raise SystemExit(
            f"error: phase '{args.value}' is not valid. One of: "
            + ", ".join(PHASES)
        )
    run_id = resolve_run_id(args)
    spath = state_path(args, run_id)
    rundir = run_dir(args, run_id)

    lock = acquire_lock(rundir)
    try:
        data = read_state(spath)
        data[args.key] = args.value
        data["updated_at"] = now_utc()
        write_state(spath, data)
    finally:
        release_lock(lock)
    print(f"set {args.key}={args.value!r} in run '{run_id}'")


def cmd_get_state(args):
    run_id = resolve_run_id(args)
    spath = state_path(args, run_id)
    data = read_state(spath)
    if args.key:
        if args.key not in data:
            raise SystemExit(f"error: run '{run_id}' has no key '{args.key}'")
        print(data[args.key])
    else:
        print(json.dumps(data, indent=2))


def cmd_record_reviewed_head(args):
    """Bind a clean worktree head to a fresh set of review verdicts."""
    if args.head and not args.no_git:
        raise SystemExit("error: --head requires --no-git (test seam only)")

    run_id = resolve_run_id(args)
    spath = state_path(args, run_id)
    data = read_state(spath)
    if data.get("phase") != "review":
        raise SystemExit(
            f"error: reviewed head can only be recorded in phase 'review'; "
            f"run '{run_id}' is at {data.get('phase')!r}"
        )

    if args.no_git:
        head = args.head or ""
    else:
        root = repo_root()
        worktree = root / data.get("worktree", "")
        rc, dirty, err = run_cmd(
            ["git", "status", "--porcelain"],
            cwd=str(worktree),
        )
        if rc != 0:
            raise SystemExit(
                f"error: cannot inspect review worktree {worktree}: "
                f"{err or dirty}"
            )
        if dirty:
            raise SystemExit(
                f"error: review worktree {worktree} is dirty; commit the exact "
                "reviewed content first"
            )
        rc, branch, err = run_cmd(
            ["git", "branch", "--show-current"],
            cwd=str(worktree),
        )
        if rc != 0 or branch != data.get("branch"):
            raise SystemExit(
                f"error: review worktree branch must be "
                f"{data.get('branch')!r}, found {branch!r}: {err}"
            )
        rc, head, err = run_cmd(
            ["git", "rev-parse", "--verify", "HEAD^{commit}"],
            cwd=str(worktree),
        )
        if rc != 0:
            raise SystemExit(
                f"error: cannot resolve review head in {worktree}: {err}"
            )

    if not re.fullmatch(r"[0-9a-fA-F]{40}", head):
        raise SystemExit(
            "error: reviewed head must be a 40-character hexadecimal commit"
        )
    head = head.lower()

    rundir = run_dir(args, run_id)
    lock = acquire_lock(rundir)
    try:
        data = read_state(spath)
        if data.get("phase") != "review":
            raise SystemExit(
                f"error: run '{run_id}' left review phase before the head "
                "could be recorded"
            )
        for gate in GATE_ORDER:
            data[f"gate_{gate}"] = ""
        data["updated_at"] = now_utc()
        write_state(spath, data)

        marker = reviewed_head_path(args, run_id)
        tmp = marker.with_name(marker.name + ".tmp")
        tmp.write_text(head + "\n", encoding="utf-8")
        os.replace(str(tmp), str(marker))
    finally:
        release_lock(lock)
    print(
        f"recorded reviewed head {head} for run '{run_id}'; "
        "cleared all review verdicts"
    )


def cmd_set_active(args):
    run_id = args.run_id
    if not state_path(args, run_id).exists():
        raise SystemExit(
            f"error: run '{run_id}' has no state.json; cannot set it active"
        )
    active_path(args).parent.mkdir(parents=True, exist_ok=True)
    active_path(args).write_text(run_id + "\n", encoding="utf-8")
    print(f"active run set to '{run_id}'")


def cmd_clear_active(args):
    ap = active_path(args)
    if ap.exists():
        ap.unlink()
        print("active run cleared")
    else:
        print("no active run to clear")


def cmd_validate_resume(args):
    """Reconcile recorded state against git/gh reality; print an action list.

    Phase is a claim, not a fact. This never trusts the recorded phase at face
    value: it probes the worktree, the branch, and the PR, then tells the
    caller what is actually true so the caller can decide the next move.
    """
    run_id = resolve_run_id(args)
    spath = state_path(args, run_id)
    data = read_state(spath)
    root = repo_root()
    worktree = root / data.get("worktree", "")
    branch = data.get("branch", "")
    pr = data.get("pr", "")

    lines = [f"reconciliation for run '{run_id}':",
             f"  recorded phase: {data.get('phase', '?')}"]

    if data.get("phase") == "done":
        lines.append(
            "  worktree: cleanup complete; absence is expected at phase done"
        )
        lines.append(
            f"  pr: {pr or 'none recorded'}"
        )
        lines.append(
            "  ACTION: resume the /work gated merge for this run; do not "
            "select another issue while its active handoff remains"
        )
        print("\n".join(lines))
        return

    # Worktree present?
    if data.get("worktree") and worktree.exists():
        lines.append(f"  worktree: present at {worktree}")
        rc, out, _ = run_cmd(["git", "status", "--porcelain"], cwd=str(worktree))
        if rc == 0:
            if out:
                lines.append("  worktree state: DIRTY (uncommitted changes) --")
                lines.append("    inspect the diff before advancing; a complete "
                             "but uncommitted tree is the classic dead-run trap")
            else:
                lines.append("  worktree state: clean")
        else:
            lines.append("  worktree state: unknown (git status failed)")
    else:
        lines.append(f"  worktree: MISSING (expected {worktree}) -- recreate it "
                     "before resuming implement/verify work")

    # Branch present?
    if branch:
        rc, _, _ = run_cmd(
            ["git", "rev-parse", "--verify", f"refs/heads/{branch}"], cwd=str(root)
        )
        if rc == 0:
            lines.append(f"  branch: {branch} exists")
        else:
            lines.append(f"  branch: {branch} MISSING -- expected for this run")

    # PR reality?
    if pr:
        rc, out, _ = run_cmd(
            ["gh", "pr", "view", pr, "--json", "state,mergeStateStatus,number"]
        )
        if rc == 0 and out:
            try:
                info = json.loads(out)
                lines.append(f"  pr #{info.get('number', pr)}: "
                             f"state={info.get('state')} "
                             f"merge={info.get('mergeStateStatus')}")
            except ValueError:
                lines.append(f"  pr {pr}: gh returned unparseable output")
        else:
            lines.append(f"  pr {pr}: gh pr view failed -- PR may be closed or gone")
    else:
        lines.append("  pr: none recorded")

    # Gate verdicts.
    open_gates = [g for g in GATE_ORDER if not data.get(f"gate_{g}")]
    if open_gates:
        lines.append("  gates outstanding: " + ", ".join(open_gates))
    else:
        lines.append("  gates: all four recorded")

    lines.append("  ACTION: resume from the earliest phase whose reality above "
                 "is incomplete; do not trust the recorded phase alone.")
    print("\n".join(lines))


def cmd_poll_pr(args):
    run_id = resolve_run_id(args)
    data = read_state(state_path(args, run_id))
    pr = data.get("pr", "")
    if not pr:
        raise SystemExit(f"error: run '{run_id}' has no PR recorded yet")
    rc, out, err = run_cmd(
        ["gh", "pr", "checks", pr, "--json", "name,state,bucket"]
    )
    if rc != 0 or not out:
        # gh pr checks exits non-zero when checks are failing/pending; fall back
        # to the plain text form so we still print something useful.
        rc2, out2, err2 = run_cmd(["gh", "pr", "checks", pr])
        print(f"pr {pr} checks (raw):")
        print(out2 or err2 or err or "no output")
        return
    try:
        checks = json.loads(out)
    except ValueError:
        print(out)
        return
    passed = pending = failed = 0
    for c in checks:
        bucket = (c.get("bucket") or c.get("state") or "").lower()
        if bucket in ("pass", "success"):
            passed += 1
        elif bucket in ("fail", "failure", "cancel", "cancelled", "error"):
            failed += 1
        else:
            pending += 1
    verdict = "FAIL" if failed else ("PENDING" if pending else "PASS")
    print(f"pr {pr}: {verdict} (pass={passed} pending={pending} fail={failed})")
    for c in checks:
        print(f"  {c.get('bucket') or c.get('state'):8} {c.get('name')}")


def cmd_cleanup_worktree(args):
    run_id = resolve_run_id(args)
    spath = state_path(args, run_id)
    data = read_state(spath)
    root = repo_root()
    worktree = root / data.get("worktree", "")

    if args.no_git:
        print("no-git: skipped 'git worktree remove'")
    elif data.get("worktree") and worktree.exists():
        rc, out, err = run_cmd(
            ["git", "worktree", "remove", str(worktree), "--force"], cwd=str(root)
        )
        if rc == 0:
            print(f"removed worktree {worktree}")
        else:
            raise SystemExit(
                f"error: worktree removal failed (rc={rc}): {err or out}; "
                "run remains active and non-done for cleanup retry"
            )
    else:
        print(f"worktree {worktree} already absent")

    rundir = run_dir(args, run_id)
    lock = acquire_lock(rundir)
    try:
        data = read_state(spath)
        data["phase"] = "done"
        data["updated_at"] = now_utc()
        write_state(spath, data)
    finally:
        release_lock(lock)
    print(f"phase set to 'done' for run '{run_id}'")
    print("active run retained for the /work merge handoff")


# --------------------------------------------------------------------------- #
# Run-id resolution: explicit --run-id, else the active marker.
# --------------------------------------------------------------------------- #

def resolve_run_id(args):
    if getattr(args, "run_id", None):
        return args.run_id
    ap = active_path(args)
    if ap.exists():
        named = ap.read_text(encoding="utf-8").strip()
        if named:
            return named
    raise SystemExit(
        "error: no run specified and no active run marker; pass --run-id or "
        "set-active first"
    )


# --------------------------------------------------------------------------- #
# Argument parsing
# --------------------------------------------------------------------------- #

def build_parser():
    p = argparse.ArgumentParser(
        prog="gh_issue_run.py",
        description="Durable run-state machine for the /gh-issue lifecycle. "
                    "The only writer of a run's flat-key state.json.",
    )
    p.add_argument(
        "--runs-dir",
        help="override the runs directory (default $SW_RUNS_DIR or "
             "<repo-root>/.agents/runs); tests use this to avoid a real run",
    )
    sub = p.add_subparsers(dest="command", required=True)

    sp = sub.add_parser("init-run", help="create a run and its worktree")
    sp.add_argument("--issue", required=True, help="GitHub issue number N")
    sp.add_argument("--slug", required=True, help="kebab-case slug for the run")
    sp.add_argument("--resume", action="store_true",
                    help="reattach to an existing run instead of refusing")
    sp.add_argument("--no-git", action="store_true",
                    help="skip 'git worktree add' (tests / authoring)")
    sp.set_defaults(func=cmd_init_run)

    sp = sub.add_parser("update-state", help="set one flat key (locked, backed up)")
    sp.add_argument("--key", required=True, help="flat key to set")
    sp.add_argument("--value", required=True, help="new value")
    sp.add_argument("--run-id", help="target run (default: the active run)")
    sp.set_defaults(func=cmd_update_state)

    sp = sub.add_parser("get-state", help="print the whole state or one key")
    sp.add_argument("--key", help="print only this key's value")
    sp.add_argument("--run-id", help="target run (default: the active run)")
    sp.set_defaults(func=cmd_get_state)

    sp = sub.add_parser("set-active", help="point the active marker at a run")
    sp.add_argument("run_id", help="run id to mark active")
    sp.set_defaults(func=cmd_set_active)

    sp = sub.add_parser("clear-active", help="remove the active marker")
    sp.set_defaults(func=cmd_clear_active)

    sp = sub.add_parser(
        "record-reviewed-head",
        help="bind a clean commit to a fresh set of review verdicts",
    )
    sp.add_argument("--run-id", help="target run (default: the active run)")
    sp.add_argument(
        "--no-git",
        action="store_true",
        help="skip worktree inspection (tests only; requires --head)",
    )
    sp.add_argument(
        "--head",
        help="reviewed commit override (tests only; requires --no-git)",
    )
    sp.set_defaults(func=cmd_record_reviewed_head)

    sp = sub.add_parser(
        "validate-resume",
        help="reconcile recorded state against git/gh reality",
    )
    sp.add_argument("--run-id", help="target run (default: the active run)")
    sp.set_defaults(func=cmd_validate_resume)

    sp = sub.add_parser("poll-pr", help="summarize the run PR's checks")
    sp.add_argument("--run-id", help="target run (default: the active run)")
    sp.set_defaults(func=cmd_poll_pr)

    sp = sub.add_parser(
        "cleanup-worktree",
        help="remove the worktree, set phase=done, retain active for /work",
    )
    sp.add_argument("--run-id", help="target run (default: the active run)")
    sp.add_argument("--no-git", action="store_true",
                    help="skip 'git worktree remove' (tests)")
    sp.set_defaults(func=cmd_cleanup_worktree)

    return p


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    args.func(args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
