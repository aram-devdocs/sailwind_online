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

The immutable companion marker `issue-url` stores the canonical GitHub issue
URL without extending this flat state schema.

Two invariants are load-bearing and MUST NOT be removed:
    1. Backup-before-write: state.json is copied to state.json.bak before any
       write, so a crashed write leaves a recoverable prior state.
    2. Single-writer lock: writes hold an operating-system advisory lock on
       .lock for the full operation, so two processes never interleave a
       read-modify-write and a crashed writer releases ownership safely.

Subcommands
-----------
    init-run --issue N --slug SLUG --issue-url URL [--resume] [--no-git]
    migrate-issue-url --run-id RUN_ID --issue-url URL
    update-state --key K --value V
    get-state [--key K]
    set-active RUN_ID
    clear-active --expected-run-id RUN_ID
    validate-resume
    record-reviewed-head [--run-id RUN_ID]
    poll-pr
    cleanup-worktree [--no-git]

Runs directory resolution (highest precedence first):
    --runs-dir ARG  >  $SW_RUNS_DIR  >  <repo-root>/.agents/runs
The override exists so tests exercise the machine without touching a real run.

Stdlib only: json, argparse, os, subprocess, time, pathlib, and the platform
file-lock module.
"""

import argparse
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path
from urllib.parse import urlparse

if os.name == "nt":
    import msvcrt
else:
    import fcntl

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

# How long to wait for a live lock before giving up.
LOCK_WAIT_SECONDS = 10

# External probes must not hang a durable run forever.
COMMAND_TIMEOUT_SECONDS = 30
PROBE_TIMEOUT_SECONDS = 5
REPOSITORY_RE = re.compile(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+")
RUN_ID_RE = re.compile(r"[1-9][0-9]*-[a-z0-9]+(?:-[a-z0-9]+)*")
REPOSITORY_CONFIG = Path(__file__).resolve().parents[3] / "repository.json"


# --------------------------------------------------------------------------- #
# Paths
# --------------------------------------------------------------------------- #

def repo_root():
    """Best-effort repo root: git first, then walk up for .agents, then cwd."""
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True, text=True, check=False,
            timeout=PROBE_TIMEOUT_SECONDS,
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


def validate_run_id(run_id):
    """Return one canonical run id or fail before it can become a path."""
    if not isinstance(run_id, str) or not RUN_ID_RE.fullmatch(run_id):
        raise SystemExit(
            f"error: invalid run id {run_id!r}; expected "
            "<positive-issue>-<lowercase-hyphen-slug>"
        )
    return run_id


def run_dir(args, run_id):
    run_id = validate_run_id(run_id)
    root = runs_dir(args).resolve(strict=False)
    candidate = (root / run_id).resolve(strict=False)
    try:
        candidate.relative_to(root)
    except ValueError as exc:
        raise SystemExit(
            f"error: invalid run id {run_id!r}; resolved path escapes {root}"
        ) from exc
    if candidate.parent != root:
        raise SystemExit(
            f"error: invalid run id {run_id!r}; run must be a direct child "
            f"of {root}"
        )
    return candidate


def state_path(args, run_id):
    return run_dir(args, run_id) / "state.json"


def reviewed_head_path(args, run_id):
    return run_dir(args, run_id) / "reviewed-head"


def issue_url_path(args, run_id):
    return run_dir(args, run_id) / "issue-url"


def active_path(args):
    return runs_dir(args) / "active"


def write_active_run_locked(args, run_id):
    """Atomically replace the active marker while the global lock is held."""
    run_id = validate_run_id(run_id)
    marker = active_path(args)
    marker.parent.mkdir(parents=True, exist_ok=True)
    tmp = marker.with_name(marker.name + ".tmp")
    tmp.write_text(run_id + "\n", encoding="utf-8")
    os.replace(str(tmp), str(marker))


def write_active_run(args, run_id):
    """Atomically replace the active marker under the global run lock."""
    lock = acquire_lock(runs_dir(args))
    try:
        write_active_run_locked(args, run_id)
    finally:
        release_lock(lock)


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

class LockHandle:
    """One process-owned advisory lock held through its open file."""

    def __init__(self, path, stream):
        self.path = path
        self.stream = stream
        self.released = False


def try_lock_file(stream):
    """Try one nonblocking exclusive lock acquisition."""
    stream.seek(0)
    if os.name == "nt":
        msvcrt.locking(stream.fileno(), msvcrt.LK_NBLCK, 1)
    else:
        fcntl.flock(stream.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)


def unlock_file(stream):
    """Release the advisory lock held by this exact open file."""
    stream.seek(0)
    if os.name == "nt":
        msvcrt.locking(stream.fileno(), msvcrt.LK_UNLCK, 1)
    else:
        fcntl.flock(stream.fileno(), fcntl.LOCK_UN)


def acquire_lock(rundir, clock=None, sleeper=None):
    """Take a bounded process-owned advisory lock."""
    if clock is None:
        clock = time.monotonic
    if sleeper is None:
        sleeper = time.sleep
    lock = rundir / ".lock"
    rundir.mkdir(parents=True, exist_ok=True)
    try:
        stream = lock.open("a+b")
        stream.seek(0, os.SEEK_END)
        if stream.tell() == 0:
            stream.write(b"\0")
            stream.flush()
    except OSError as exc:
        raise SystemExit(f"error: cannot open lock {lock}: {exc}") from exc

    deadline = clock() + LOCK_WAIT_SECONDS
    while True:
        try:
            try_lock_file(stream)
            return LockHandle(lock, stream)
        except OSError:
            if clock() >= deadline:
                stream.close()
                raise SystemExit(
                    f"error: could not acquire {lock}; another writer is active"
                )
            sleeper(0.2)


def release_lock(lock):
    if not isinstance(lock, LockHandle):
        raise TypeError("release_lock requires the owning LockHandle")
    if lock.released:
        return
    try:
        unlock_file(lock.stream)
    finally:
        lock.stream.close()
        lock.released = True


# --------------------------------------------------------------------------- #
# State read / write
# --------------------------------------------------------------------------- #

def now_utc():
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def trusted_repository():
    """Read and strictly validate the tracked GitHub repository identity."""
    try:
        data = json.loads(REPOSITORY_CONFIG.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise SystemExit(
            f"error: cannot read tracked repository identity at "
            f"{REPOSITORY_CONFIG}: {exc}"
        ) from exc
    if (
        not isinstance(data, dict)
        or set(data) != {"repository"}
        or not isinstance(data["repository"], str)
    ):
        raise SystemExit(
            "error: tracked repository identity must be an object containing "
            "only a string 'repository' field"
        )
    repository = data["repository"]
    if not REPOSITORY_RE.fullmatch(repository) or any(
        part in (".", "..") for part in repository.split("/")
    ):
        raise SystemExit(
            f"error: tracked repository identity is invalid: {repository!r}"
        )
    return repository


def parse_issue_url(value, expected_issue):
    """Validate a canonical GitHub issue URL and return owner/repository."""
    if not isinstance(value, str):
        raise SystemExit("error: --issue-url must be a canonical GitHub URL")
    parsed = urlparse(value)
    parts = [part for part in parsed.path.split("/") if part]
    canonical = ""
    if len(parts) == 4:
        canonical = (
            f"https://github.com/{parts[0]}/{parts[1]}/issues/"
            f"{expected_issue}"
        )
    if (
        parsed.scheme != "https"
        or parsed.netloc != "github.com"
        or parsed.query
        or parsed.fragment
        or len(parts) != 4
        or parts[2] != "issues"
        or parts[3] != str(expected_issue)
        or not re.fullmatch(r"[A-Za-z0-9_.-]+", parts[0])
        or not re.fullmatch(r"[A-Za-z0-9_.-]+", parts[1])
        or parts[0] in (".", "..")
        or parts[1] in (".", "..")
        or value != canonical
    ):
        raise SystemExit(
            f"error: --issue-url must be the canonical GitHub URL for issue "
            f"#{expected_issue}, found {value!r}"
        )
    repository = f"{parts[0]}/{parts[1]}"
    configured = trusted_repository()
    if repository != configured:
        raise SystemExit(
            f"error: issue URL repository {repository!r} does not match "
            f"tracked repository {configured!r}"
        )
    return repository


def write_issue_url_marker(path, issue_url):
    """Atomically create one immutable issue URL marker."""
    expected = issue_url + "\n"
    if path.exists():
        try:
            recorded = path.read_text(encoding="utf-8")
        except OSError as exc:
            raise SystemExit(
                f"error: cannot read issue URL marker at {path}: {exc}"
            ) from exc
        if recorded == expected:
            return False
        raise SystemExit(
            f"error: issue URL marker at {path} is immutable and already "
            "contains a different value"
        )
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    try:
        tmp.write_text(expected, encoding="utf-8")
        os.replace(str(tmp), str(path))
    except OSError as exc:
        raise SystemExit(
            f"error: cannot atomically write issue URL marker at {path}: {exc}"
        ) from exc
    return True


def read_issue_url_marker(args, run_id, expected_issue):
    """Read and validate the immutable companion repository identity."""
    marker = issue_url_path(args, run_id)
    try:
        raw = marker.read_text(encoding="utf-8")
    except OSError as exc:
        raise SystemExit(
            f"error: missing or unreadable issue URL marker at {marker}: {exc}; "
            "use migrate-issue-url with an independently recorded GitHub URL"
        ) from exc
    if not raw or raw != raw.strip() + "\n" or "\n" in raw[:-1]:
        raise SystemExit(
            f"error: issue URL marker at {marker} must contain exactly one "
            "canonical URL line"
        )
    issue_url = raw[:-1]
    parse_issue_url(issue_url, expected_issue)
    return issue_url


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


def read_state(path, allow_legacy_issue_url=False):
    """Load state.json as a dict. Raises SystemExit with a clear message."""
    if not path.exists():
        raise SystemExit(f"error: no state at {path}")
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise SystemExit(f"error: cannot read {path}: {exc}")
    if not isinstance(data, dict):
        raise SystemExit(f"error: {path} is not a flat object")
    if allow_legacy_issue_url:
        keys = set(data)
        allowed = set(ALLOWED_KEYS)
        if keys not in (allowed, allowed | {"issue_url"}):
            raise SystemExit(
                f"error: legacy state keys are invalid; "
                f"missing={sorted(allowed - keys)}, "
                f"extra={sorted(keys - allowed)}"
            )
        for key, value in data.items():
            if not isinstance(value, str):
                raise SystemExit(
                    f"error: legacy key {key!r} has non-string value "
                    f"{value!r}"
                )
    else:
        validate_flat(data)
    return data


def validate_flat(data):
    """Every value MUST be a string (flat contract). Reject nested structures."""
    if set(data) != set(ALLOWED_KEYS):
        missing = sorted(set(ALLOWED_KEYS) - set(data))
        extra = sorted(set(data) - set(ALLOWED_KEYS))
        if not missing and extra == ["issue_url"]:
            raise SystemExit(
                "error: legacy state.json contains issue_url; use "
                "migrate-issue-url to move it into the companion marker"
            )
        raise SystemExit(
            f"error: state keys must exactly match the flat contract; "
            f"missing={missing}, extra={extra}"
        )
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
    run_id = validate_run_id(f"{args.issue}-{args.slug}")
    rundir = run_dir(args, run_id)
    spath = state_path(args, run_id)

    global_lock = acquire_lock(runs_dir(args))
    lock = None
    try:
        lock = acquire_lock(rundir)
        if spath.exists() and not args.resume:
            raise SystemExit(
                f"error: run '{run_id}' already exists at {spath}; pass "
                "--resume to reattach to it"
        )
        if spath.exists() and args.resume:
            data = read_state(spath, allow_legacy_issue_url=True)
            if "issue_url" in data:
                raise SystemExit(
                    f"error: legacy run '{run_id}' stores issue_url in "
                    "state.json; use migrate-issue-url to move it into the "
                    "companion marker"
                )
            validate_flat(data)
            recorded_url = read_issue_url_marker(
                args,
                run_id,
                args.issue,
            )
            if args.issue_url and args.issue_url != recorded_url:
                raise SystemExit(
                    f"error: --issue-url {args.issue_url!r} does not match "
                    f"recorded identity {recorded_url!r}"
                )
            data["updated_at"] = now_utc()
            write_state(spath, data)
            print(f"resumed existing run '{run_id}' at phase '{data.get('phase')}'")
        else:
            if not args.issue_url:
                raise SystemExit(
                    "error: --issue-url is required when initializing a run"
                )
            parse_issue_url(args.issue_url, args.issue)
            data = blank_state(
                run_id,
                args.issue,
                args.slug,
            )
            write_issue_url_marker(
                issue_url_path(args, run_id),
                args.issue_url,
            )
            write_state(spath, data)
            print(f"initialized run '{run_id}' (phase=investigate) at {spath}")

        # Mark this run active for the hooks.
        write_active_run_locked(args, run_id)
        print(f"active run set to '{run_id}'")

        # Create the isolated worktree while the lifecycle lock excludes cleanup.
        if data.get("phase") in ("cleanup", "done"):
            print(
                f"run '{run_id}' is at phase {data.get('phase')!r}; "
                "worktree recreation is not valid during or after cleanup"
            )
            return
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
    finally:
        if lock is not None:
            release_lock(lock)
        release_lock(global_lock)


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


def cmd_migrate_issue_url(args):
    """Move legacy identity into an immutable companion marker."""
    run_id = resolve_run_id(args)
    global_lock = acquire_lock(runs_dir(args))
    lock = None
    try:
        rundir = run_dir(args, run_id)
        lock = acquire_lock(rundir)
        spath = state_path(args, run_id)
        data = read_state(spath, allow_legacy_issue_url=True)
        issue = data.get("issue", "")
        if not issue.isdigit() or int(issue) <= 0:
            raise SystemExit(
                f"error: legacy run {run_id!r} has invalid issue {issue!r}"
            )
        if not run_id.startswith(issue + "-"):
            raise SystemExit(
                f"error: legacy run {run_id!r} does not match issue "
                f"{issue!r}"
            )
        parse_issue_url(args.issue_url, issue)
        legacy_url = data.get("issue_url", "")
        if legacy_url:
            parse_issue_url(legacy_url, issue)
        if legacy_url and legacy_url != args.issue_url:
            raise SystemExit(
                f"error: legacy run already records issue_url {legacy_url!r}; "
                "repository identity is immutable"
            )
        marker = issue_url_path(args, run_id)
        marker_exists = marker.exists()
        if marker_exists:
            recorded = read_issue_url_marker(args, run_id, issue)
            if recorded != args.issue_url:
                raise SystemExit(
                    f"error: issue URL marker already records {recorded!r}; "
                    "repository identity is immutable"
                )
        if data.get("phase") != "done":
            active = active_path(args)
            try:
                active_run = active.read_text(encoding="utf-8").strip()
            except OSError as exc:
                raise SystemExit(
                    f"error: active legacy migration requires readable "
                    f"{active}: {exc}"
                ) from exc
            if active_run != run_id:
                raise SystemExit(
                    f"error: active marker names {active_run!r}, not legacy "
                    f"run {run_id!r}"
                )
            recorded_worktree = data.get("worktree", "")
            recorded_branch = data.get("branch", "")
            if (
                Path(recorded_worktree) != Path(".worktrees") / run_id
                or recorded_branch != f"feat/{run_id}"
            ):
                raise SystemExit(
                    f"error: active legacy run has mismatched worktree or "
                    f"branch identity: {recorded_worktree!r}/"
                    f"{recorded_branch!r}"
                )
            worktree = repo_root() / recorded_worktree
            if not worktree.is_dir():
                raise SystemExit(
                    f"error: active legacy worktree is missing at {worktree}"
                )
            rc, dirty, err = run_cmd(
                ["git", "status", "--porcelain"],
                cwd=str(worktree),
            )
            if rc != 0 or dirty:
                raise SystemExit(
                    f"error: active legacy worktree must be clean before "
                    f"migration: {err or dirty}"
                )
            rc, branch, err = run_cmd(
                ["git", "branch", "--show-current"],
                cwd=str(worktree),
            )
            if rc != 0 or branch != recorded_branch:
                raise SystemExit(
                    f"error: active legacy worktree branch must be "
                    f"{recorded_branch!r}, found {branch!r}: {err}"
                )
        marker_created = write_issue_url_marker(marker, args.issue_url)
        state_migrated = "issue_url" in data
        if state_migrated:
            data.pop("issue_url")
        if marker_created or state_migrated:
            data["updated_at"] = now_utc()
            write_state(spath, data)
        else:
            print(f"issue URL already recorded for run '{run_id}'")
            return
    finally:
        if lock is not None:
            release_lock(lock)
        release_lock(global_lock)
    print(f"migrated issue URL for legacy run '{run_id}'")


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
    run_id = validate_run_id(args.run_id)
    if not state_path(args, run_id).exists():
        raise SystemExit(
            f"error: run '{run_id}' has no state.json; cannot set it active"
        )
    write_active_run(args, run_id)
    print(f"active run set to '{run_id}'")


def cmd_clear_active(args):
    expected_run_id = validate_run_id(args.expected_run_id)
    root = runs_dir(args)
    lock = acquire_lock(root)
    try:
        ap = active_path(args)
        if not ap.exists():
            print("no active run to clear")
            return
        try:
            named = ap.read_text(encoding="utf-8").strip()
        except OSError as exc:
            raise SystemExit(f"error: cannot read active run marker: {exc}")
        named = validate_run_id(named)
        if named != expected_run_id:
            raise SystemExit(
                f"error: active run changed to {named!r}; expected "
                f"{expected_run_id!r}, so the replacement was preserved"
            )
        try:
            ap.unlink()
        except OSError as exc:
            raise SystemExit(f"error: cannot clear active run marker: {exc}")
        print(f"active run '{expected_run_id}' cleared")
    finally:
        release_lock(lock)


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
        marker = issue_url_path(args, run_id)
        if not marker.is_file():
            lines.append(
                "  repository: MISSING durable issue-url marker for legacy run"
            )
            lines.append(
                "  ACTION: run migrate-issue-url with an independently "
                "recorded canonical GitHub issue URL before gated merge"
            )
            print("\n".join(lines))
            return
        read_issue_url_marker(args, run_id, data.get("issue", ""))
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

    repo = parse_issue_url(
        read_issue_url_marker(
            args,
            run_id,
            data.get("issue", ""),
        ),
        data.get("issue", ""),
    )

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
            [
                "gh",
                "pr",
                "view",
                pr,
                "--repo",
                repo,
                "--json",
                "state,mergeStateStatus,number",
            ]
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
    repo = parse_issue_url(
        read_issue_url_marker(
            args,
            run_id,
            data.get("issue", ""),
        ),
        data.get("issue", ""),
    )
    rc, out, err = run_cmd(
        [
            "gh",
            "pr",
            "checks",
            pr,
            "--repo",
            repo,
            "--json",
            "name,state,bucket",
        ]
    )
    if rc != 0 or not out:
        # gh pr checks exits non-zero when checks are failing/pending; fall back
        # to the plain text form so we still print something useful.
        rc2, out2, err2 = run_cmd(
            ["gh", "pr", "checks", pr, "--repo", repo]
        )
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


def normalized_path(path):
    """Return a case-normalized absolute path for exact worktree matching."""
    return os.path.normcase(str(Path(path).resolve(strict=False)))


def registered_worktrees(root):
    """Parse the stable Git porcelain format into full worktree entries."""
    rc, out, err = run_cmd(
        ["git", "worktree", "list", "--porcelain"],
        cwd=str(root),
    )
    if rc != 0:
        raise SystemExit(
            f"error: worktree registry inspection failed (rc={rc}): "
            f"{err or out}"
        )
    entries = {}
    current = None
    for line in (*out.splitlines(), ""):
        if not line:
            if current is not None:
                key = normalized_path(current["path"])
                if key in entries:
                    raise SystemExit(
                        f"error: worktree registry contains duplicate path "
                        f"{current['path']!r}"
                    )
                entries[key] = current
                current = None
            continue
        if line.startswith("worktree "):
            if current is not None:
                raise SystemExit(
                    "error: malformed worktree registry: missing entry separator"
                )
            current = {
                "path": line.removeprefix("worktree "),
                "head": "",
                "branch": "",
            }
            continue
        if current is None:
            raise SystemExit(
                f"error: malformed worktree registry line {line!r}"
            )
        if line.startswith("HEAD "):
            current["head"] = line.removeprefix("HEAD ")
        elif line.startswith("branch "):
            current["branch"] = line.removeprefix("branch ")
        elif line == "detached":
            current["detached"] = True
        elif line == "bare":
            current["bare"] = True
        elif line == "locked" or line.startswith("locked "):
            current["locked"] = line.removeprefix("locked").strip()
        elif line == "prunable" or line.startswith("prunable "):
            current["prunable"] = line.removeprefix("prunable").strip()
        else:
            raise SystemExit(
                f"error: unrecognized worktree registry line {line!r}"
            )
    return entries


def validate_cleanup_entry(args, run_id, data, entry):
    """Bind a destructive worktree removal to branch and reviewed commit."""
    expected_branch = f"refs/heads/{data.get('branch', '')}"
    marker = reviewed_head_path(args, run_id)
    try:
        expected_head = marker.read_text(encoding="utf-8").strip().lower()
    except OSError as exc:
        raise SystemExit(
            f"error: cannot read reviewed head for cleanup: {exc}"
        ) from exc
    actual_head = entry.get("head", "").lower()
    actual_branch = entry.get("branch", "")
    if (
        not re.fullmatch(r"[0-9a-f]{40}", expected_head)
        or actual_head != expected_head
        or actual_branch != expected_branch
    ):
        raise SystemExit(
            f"error: worktree identity mismatch for {entry.get('path')!r}: "
            f"expected {expected_branch} at {expected_head!r}, found "
            f"{actual_branch!r} at {actual_head!r}; refusing removal"
        )


def require_clean_worktree(worktree):
    """Fail unless the exact worktree has no tracked or untracked changes."""
    rc, out, err = run_cmd(
        [
            "git",
            "status",
            "--porcelain",
            "--untracked-files=all",
        ],
        cwd=str(worktree),
    )
    if rc != 0:
        raise SystemExit(
            f"error: cannot verify worktree cleanliness (rc={rc}): "
            f"{err or out}; run remains active and non-done for cleanup retry"
        )
    if out:
        raise SystemExit(
            f"error: worktree is not clean after review: {out}; "
            "run remains active and non-done for cleanup retry"
        )


def cmd_cleanup_worktree(args):
    run_id = resolve_run_id(args)
    rundir = run_dir(args, run_id)
    spath = state_path(args, run_id)
    root = repo_root()
    global_lock = acquire_lock(runs_dir(args))
    lock = None
    try:
        lock = acquire_lock(rundir)
        data = read_state(spath)
        recorded_worktree = data.get("worktree", "")
        expected_worktree = Path(".worktrees") / run_id
        if (
            not recorded_worktree
            or Path(recorded_worktree).is_absolute()
            or Path(recorded_worktree) != expected_worktree
            or data.get("branch") != f"feat/{run_id}"
        ):
            raise SystemExit(
                f"error: recorded worktree identity "
                f"{recorded_worktree!r}/{data.get('branch')!r} does not match "
                f"the exact run {run_id!r}"
            )
        worktree = root / recorded_worktree

        if args.no_git:
            print("no-git: skipped 'git worktree remove'")
        else:
            target = normalized_path(worktree)
            entries = registered_worktrees(root)
            entry = entries.get(target)
            if entry is not None:
                validate_cleanup_entry(args, run_id, data, entry)
                worktree_present = worktree.exists()
                if worktree_present:
                    require_clean_worktree(worktree)
                    remove_command = [
                        "git",
                        "worktree",
                        "remove",
                        str(worktree),
                    ]
                else:
                    remove_command = [
                        "git",
                        "worktree",
                        "remove",
                        str(worktree),
                        "--force",
                    ]
                rc, out, err = run_cmd(remove_command, cwd=str(root))
                if rc != 0:
                    raise SystemExit(
                        f"error: worktree removal failed (rc={rc}): "
                        f"{err or out}; run remains active and non-done for "
                        "cleanup retry"
                    )
                if worktree_present:
                    print(f"removed worktree {worktree}")
                else:
                    print(
                        f"removed stale worktree registration for {worktree}"
                    )
            elif worktree.exists():
                raise SystemExit(
                    f"error: worktree path {worktree} exists but is not "
                    "registered; refusing to remove an unowned directory"
                )
            else:
                print(f"worktree {worktree} already absent")

            after_remove = getattr(args, "_after_remove_hook", None)
            if after_remove is not None:
                after_remove()

            if target in registered_worktrees(root):
                raise SystemExit(
                    f"error: worktree {worktree} remains registered after "
                    "cleanup; run remains active and non-done for cleanup retry"
                )
            if worktree.exists():
                raise SystemExit(
                    f"error: worktree path {worktree} remains after cleanup; "
                    "run remains active and non-done for cleanup retry"
                )

        data = read_state(spath)
        data["phase"] = "done"
        data["updated_at"] = now_utc()
        write_state(spath, data)
    finally:
        if lock is not None:
            release_lock(lock)
        release_lock(global_lock)
    print(f"phase set to 'done' for run '{run_id}'")
    print("active run retained for the /work merge handoff")


# --------------------------------------------------------------------------- #
# Run-id resolution: explicit --run-id, else the active marker.
# --------------------------------------------------------------------------- #

def resolve_run_id(args):
    if getattr(args, "run_id", None):
        return validate_run_id(args.run_id)
    ap = active_path(args)
    if ap.exists():
        named = ap.read_text(encoding="utf-8").strip()
        if named:
            return validate_run_id(named)
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
    sp.add_argument(
        "--issue-url",
        help="canonical https://github.com/<owner>/<repo>/issues/<N> URL",
    )
    sp.add_argument("--resume", action="store_true",
                    help="reattach to an existing run instead of refusing")
    sp.add_argument("--no-git", action="store_true",
                    help="skip 'git worktree add' (tests / authoring)")
    sp.set_defaults(func=cmd_init_run)

    sp = sub.add_parser(
        "migrate-issue-url",
        help="bind a legacy run missing identity to an explicit issue URL",
    )
    sp.add_argument("--run-id", required=True, help="legacy run id")
    sp.add_argument(
        "--issue-url",
        required=True,
        help="canonical https://github.com/<owner>/<repo>/issues/<N> URL",
    )
    sp.set_defaults(func=cmd_migrate_issue_url)

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

    sp = sub.add_parser(
        "clear-active",
        help="remove the active marker only when it names the expected run",
    )
    sp.add_argument(
        "--expected-run-id",
        required=True,
        help="run id that must still own the active marker",
    )
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
