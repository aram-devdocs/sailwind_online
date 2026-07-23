#!/usr/bin/env python3
"""Squash-merge one completed /work run after rechecking every gate.

This script reads the existing /gh-issue flat state without modifying it.
GitHub access is isolated behind a command runner so unit tests never call gh.
"""

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import NamedTuple
from urllib.parse import urlparse


GATES = ("spec", "quality", "architecture", "security")
PR_FIELDS = (
    "number,state,isDraft,baseRefName,headRefName,headRefOid,"
    "mergeStateStatus"
)
CLOSURE_QUERY = (
    "query($owner:String!,$name:String!,$number:Int!){"
    "repository(owner:$owner,name:$name){pullRequest(number:$number){"
    "closingIssuesReferences(first:100){nodes{number "
    "repository{nameWithOwner}}}}}}"
)
RUN_ID_RE = re.compile(r"(?P<issue>[1-9][0-9]*)-[a-z0-9]+(?:-[a-z0-9]+)*")
HEAD_OID_RE = re.compile(r"[0-9a-fA-F]{40}")


class MergePreconditionError(RuntimeError):
    """A failed gate that forbids invoking the merge command."""


class MergeConfirmationError(RuntimeError):
    """A failed merge command or post-merge confirmation."""


class MergeResult(NamedTuple):
    pr_number: int
    issue_number: int
    head_oid: str
    merged: bool


def run_command(command):
    """Run one command without raising so callers can report exact failures."""
    try:
        return subprocess.run(
            command,
            capture_output=True,
            text=True,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        return subprocess.CompletedProcess(command, 127, "", str(exc))


def read_json_result(result, purpose, error_type, allow_nonzero=False):
    """Parse a gh JSON response and preserve useful command failure details."""
    if result.returncode != 0 and not allow_nonzero:
        detail = result.stderr.strip() or result.stdout.strip() or "no output"
        raise error_type(f"{purpose} failed: {detail}")
    if not result.stdout.strip():
        detail = result.stderr.strip() or "gh returned no JSON"
        raise error_type(f"{purpose} failed: {detail}")
    try:
        return json.loads(result.stdout)
    except ValueError as exc:
        raise error_type(f"{purpose} returned invalid JSON: {exc}") from exc


def load_state(runs_dir, run_id):
    """Load and locally validate the immutable completed-run identity."""
    match = RUN_ID_RE.fullmatch(run_id)
    if not match:
        raise MergePreconditionError(
            f"recorded run id is invalid: {run_id!r}"
        )
    state_path = Path(runs_dir) / run_id / "state.json"
    try:
        state = json.loads(state_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        raise MergePreconditionError(
            f"cannot read completed run state at {state_path}: {exc}"
        ) from exc
    if not isinstance(state, dict) or any(
        not isinstance(value, str) for value in state.values()
    ):
        raise MergePreconditionError(
            "completed run state must follow the flat string-value contract"
        )
    if state.get("run_id") != run_id:
        raise MergePreconditionError(
            f"state run_id {state.get('run_id')!r} does not match {run_id!r}"
        )
    issue = state.get("issue", "")
    if issue != match.group("issue"):
        raise MergePreconditionError(
            f"recorded issue {issue!r} does not match run {run_id!r}"
        )
    if state.get("branch") != f"feat/{run_id}":
        raise MergePreconditionError(
            f"recorded branch {state.get('branch')!r} does not match run "
            f"{run_id!r}"
        )
    if state.get("phase") != "done":
        raise MergePreconditionError(
            f"run phase must be 'done', found {state.get('phase')!r}"
        )
    if state.get("plan_open") != "0":
        raise MergePreconditionError(
            f"plan_open must be '0', found {state.get('plan_open')!r}"
        )
    for gate in GATES:
        verdict = state.get(f"gate_{gate}")
        if verdict != "APPROVE":
            raise MergePreconditionError(
                f"{gate} gate must be APPROVE, found {verdict!r}"
            )
    return state


def parse_recorded_pr(value):
    """Return the recorded PR number and an optional repository from its URL."""
    if value.isdigit() and int(value) > 0:
        return int(value), None
    parsed = urlparse(value)
    parts = [part for part in parsed.path.split("/") if part]
    if (
        parsed.scheme == "https"
        and parsed.netloc == "github.com"
        and len(parts) == 4
        and parts[2] == "pull"
        and parts[3].isdigit()
        and int(parts[3]) > 0
    ):
        return int(parts[3]), f"{parts[0]}/{parts[1]}"
    raise MergePreconditionError(
        f"recorded PR must be a positive number or GitHub PR URL, found {value!r}"
    )


def repository_name(runner):
    result = runner(["gh", "repo", "view", "--json", "nameWithOwner"])
    data = read_json_result(
        result,
        "repository lookup",
        MergePreconditionError,
    )
    repo = data.get("nameWithOwner") if isinstance(data, dict) else None
    if (
        not isinstance(repo, str)
        or repo.count("/") != 1
        or any(not part for part in repo.split("/"))
    ):
        raise MergePreconditionError(
            f"repository lookup returned invalid nameWithOwner: {repo!r}"
        )
    return repo


def pr_view_command(pr_number, repo):
    return [
        "gh",
        "pr",
        "view",
        str(pr_number),
        "--repo",
        repo,
        "--json",
        PR_FIELDS,
    ]


def load_pr(runner, pr_number, repo):
    result = runner(pr_view_command(pr_number, repo))
    data = read_json_result(
        result,
        f"PR #{pr_number} lookup",
        MergePreconditionError,
    )
    if not isinstance(data, dict):
        raise MergePreconditionError(
            f"PR #{pr_number} lookup returned a non-object"
        )
    return data


def validate_pr(pr, pr_number, branch):
    """Validate PR identity, mergeability, and head."""
    if pr.get("number") != pr_number:
        raise MergePreconditionError(
            f"PR lookup returned #{pr.get('number')!r}, expected #{pr_number}"
        )
    if pr.get("state") != "OPEN":
        raise MergePreconditionError(
            f"PR #{pr_number} must be OPEN, found {pr.get('state')!r}"
        )
    if pr.get("isDraft") is not False:
        raise MergePreconditionError(f"PR #{pr_number} must not be a draft")
    if pr.get("baseRefName") != "dev":
        raise MergePreconditionError(
            f"PR #{pr_number} must target dev, found {pr.get('baseRefName')!r}"
        )
    if pr.get("headRefName") != branch:
        raise MergePreconditionError(
            f"PR #{pr_number} head must be {branch!r}, found "
            f"{pr.get('headRefName')!r}"
        )
    if pr.get("mergeStateStatus") != "CLEAN":
        raise MergePreconditionError(
            f"PR #{pr_number} merge state must be CLEAN, found "
            f"{pr.get('mergeStateStatus')!r}"
        )
    head_oid = pr.get("headRefOid")
    if not isinstance(head_oid, str) or not HEAD_OID_RE.fullmatch(head_oid):
        raise MergePreconditionError(
            f"PR #{pr_number} returned invalid head commit {head_oid!r}"
        )
    return head_oid


def validate_closure_reference(
    runner,
    pr_number,
    issue_number,
    repo,
):
    """Require GraphQL to report the exact recorded issue as a closing link."""
    owner, name = repo.split("/", 1)
    command = [
        "gh",
        "api",
        "graphql",
        "-f",
        f"query={CLOSURE_QUERY}",
        "-F",
        f"owner={owner}",
        "-F",
        f"name={name}",
        "-F",
        f"number={pr_number}",
    ]
    result = runner(command)
    response = read_json_result(
        result,
        f"closure references for PR #{pr_number}",
        MergePreconditionError,
    )
    if not isinstance(response, dict):
        raise MergePreconditionError(
            f"closure references for PR #{pr_number} returned a non-object"
        )
    if response.get("errors"):
        raise MergePreconditionError(
            f"closure references for PR #{pr_number} returned GraphQL errors: "
            f"{response['errors']!r}"
        )
    try:
        nodes = response["data"]["repository"]["pullRequest"][
            "closingIssuesReferences"
        ]["nodes"]
    except (KeyError, TypeError) as exc:
        raise MergePreconditionError(
            f"closure references for PR #{pr_number} returned an unexpected "
            "GraphQL shape"
        ) from exc
    if not isinstance(nodes, list):
        raise MergePreconditionError(
            f"closure references for PR #{pr_number} returned non-list nodes"
        )
    linked = any(
        isinstance(node, dict)
        and node.get("number") == issue_number
        and isinstance(node.get("repository"), dict)
        and node["repository"].get("nameWithOwner") == repo
        for node in nodes
    )
    if not linked:
        raise MergePreconditionError(
            f"PR #{pr_number} does not link recorded issue #{issue_number} "
            f"in {repo} for closure"
        )


def validate_required_checks(runner, pr_number, repo):
    """Require every required check and the aggregate CI gate to pass."""
    command = [
        "gh",
        "pr",
        "checks",
        str(pr_number),
        "--repo",
        repo,
        "--required",
        "--json",
        "name,state,bucket",
    ]
    result = runner(command)
    checks = read_json_result(
        result,
        f"required checks for PR #{pr_number}",
        MergePreconditionError,
        allow_nonzero=True,
    )
    if not isinstance(checks, list) or not checks:
        raise MergePreconditionError(
            f"PR #{pr_number} has no required checks to prove the current head"
        )
    not_passed = []
    for check in checks:
        if not isinstance(check, dict):
            not_passed.append(f"<malformed>={check!r}")
        elif check.get("bucket") != "pass":
            name = check.get("name", "<unnamed>")
            status = check.get("bucket") or check.get("state")
            not_passed.append(f"{name}={status}")
    if not_passed:
        raise MergePreconditionError(
            f"PR #{pr_number} required checks are not all passed: "
            + ", ".join(not_passed)
        )
    if not any(check.get("name") == "gate" for check in checks):
        raise MergePreconditionError(
            f"PR #{pr_number} required checks do not include the CI gate that "
            "mirrors make validate"
        )


def confirm_merged(runner, pr_number, issue_number, repo):
    """Confirm GitHub recorded both promised postconditions."""
    pr_result = runner(
        [
            "gh",
            "pr",
            "view",
            str(pr_number),
            "--repo",
            repo,
            "--json",
            "number,state,mergedAt",
        ]
    )
    pr = read_json_result(
        pr_result,
        f"post-merge PR #{pr_number} confirmation",
        MergeConfirmationError,
    )
    if (
        not isinstance(pr, dict)
        or pr.get("number") != pr_number
        or pr.get("state") != "MERGED"
        or not pr.get("mergedAt")
    ):
        raise MergeConfirmationError(
            f"post-merge PR confirmation failed: expected #{pr_number} MERGED, "
            f"found {pr!r}"
        )

    issue_result = runner(
        [
            "gh",
            "issue",
            "view",
            str(issue_number),
            "--repo",
            repo,
            "--json",
            "number,state",
        ]
    )
    issue = read_json_result(
        issue_result,
        f"post-merge issue #{issue_number} confirmation",
        MergeConfirmationError,
    )
    if (
        not isinstance(issue, dict)
        or issue.get("number") != issue_number
        or issue.get("state") != "CLOSED"
    ):
        raise MergeConfirmationError(
            f"post-merge issue confirmation failed: expected #{issue_number} "
            f"CLOSED, found {issue!r}"
        )


def merge_completed_run(runs_dir, run_id, runner=run_command, dry_run=False):
    """Validate, squash-merge, delete the branch, and confirm closure."""
    state = load_state(runs_dir, run_id)
    issue_number = int(state["issue"])
    pr_number, recorded_repo = parse_recorded_pr(state.get("pr", ""))
    repo = repository_name(runner)
    if recorded_repo is not None and recorded_repo != repo:
        raise MergePreconditionError(
            f"recorded PR repository {recorded_repo!r} does not match "
            f"current repository {repo!r}"
        )

    first_pr = load_pr(runner, pr_number, repo)
    head_oid = validate_pr(
        first_pr,
        pr_number,
        state["branch"],
    )
    validate_closure_reference(
        runner,
        pr_number,
        issue_number,
        repo,
    )
    validate_required_checks(runner, pr_number, repo)

    final_pr = load_pr(runner, pr_number, repo)
    final_head = final_pr.get("headRefOid")
    if final_head != head_oid:
        raise MergePreconditionError(
            f"PR #{pr_number} head changed from {head_oid} to {final_head}; "
            "checks must pass again on the new head"
        )
    validate_pr(final_pr, pr_number, state["branch"])
    validate_closure_reference(
        runner,
        pr_number,
        issue_number,
        repo,
    )

    if dry_run:
        return MergeResult(pr_number, issue_number, head_oid, False)

    merge_command = [
        "gh",
        "pr",
        "merge",
        str(pr_number),
        "--repo",
        repo,
        "--squash",
        "--delete-branch",
        "--match-head-commit",
        head_oid,
    ]
    merge_result = runner(merge_command)
    if merge_result.returncode != 0:
        detail = (
            merge_result.stderr.strip()
            or merge_result.stdout.strip()
            or "no output"
        )
        raise MergeConfirmationError(
            f"squash merge of PR #{pr_number} failed: {detail}"
        )

    confirm_merged(runner, pr_number, issue_number, repo)
    return MergeResult(pr_number, issue_number, head_oid, True)


def build_parser():
    parser = argparse.ArgumentParser(
        description=(
            "Squash-merge a completed /work run after rechecking its recorded "
            "state, PR identity, mergeability, and required checks."
        )
    )
    parser.add_argument(
        "--runs-dir",
        type=Path,
        default=Path(".agents/runs"),
        help="run-state directory (default: .agents/runs)",
    )
    parser.add_argument(
        "--run-id",
        required=True,
        help="completed run id, such as 48-gated-self-merge",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="check every precondition but do not invoke or confirm a merge",
    )
    return parser


def main(argv=None):
    args = build_parser().parse_args(argv)
    try:
        result = merge_completed_run(
            args.runs_dir,
            args.run_id,
            dry_run=args.dry_run,
        )
    except MergePreconditionError as exc:
        print(f"STOP: {exc}", file=sys.stderr)
        return 1
    except MergeConfirmationError as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 2
    if result.merged:
        print(
            f"MERGED: PR #{result.pr_number} at {result.head_oid}; "
            f"issue #{result.issue_number} is CLOSED"
        )
    else:
        print(
            f"READY: PR #{result.pr_number} at {result.head_oid} satisfies "
            "every merge precondition; dry-run made no changes"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
