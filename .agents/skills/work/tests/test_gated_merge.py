import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "gated_merge.py"
SPEC = importlib.util.spec_from_file_location("gated_merge", SCRIPT)
gated_merge = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gated_merge)


class FakeRunner:
    def __init__(self, responses):
        self.responses = list(responses)
        self.calls = []

    def __call__(self, command):
        self.calls.append(command)
        if not self.responses:
            raise AssertionError(f"unexpected command: {command}")
        expected, returncode, stdout, stderr = self.responses.pop(0)
        if command != expected:
            raise AssertionError(f"expected {expected}, got {command}")
        if not isinstance(stdout, str):
            stdout = json.dumps(stdout)
        return subprocess.CompletedProcess(command, returncode, stdout, stderr)

    def assert_finished(self):
        if self.responses:
            raise AssertionError(f"unused responses: {self.responses}")


class GatedMergeTests(unittest.TestCase):
    run_id = "48-gated-self-merge"
    repo = "aram-devdocs/sailwind_online"
    head = "a" * 40

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.runs_dir = Path(self.temp.name)
        self.write_state()
        self.write_reviewed_head()
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )

    def tearDown(self):
        self.temp.cleanup()

    def write_state(self, **overrides):
        state = {
            "run_id": self.run_id,
            "issue": "48",
            "phase": "done",
            "branch": "feat/48-gated-self-merge",
            "worktree": ".worktrees/48-gated-self-merge",
            "pr": "52",
            "gate_spec": "APPROVE",
            "gate_quality": "APPROVE",
            "gate_architecture": "APPROVE",
            "gate_security": "APPROVE",
            "plan_open": "0",
            "updated_at": "2026-07-23T12:00:00Z",
        }
        state.update(overrides)
        run_dir = self.runs_dir / self.run_id
        run_dir.mkdir(parents=True, exist_ok=True)
        (run_dir / "state.json").write_text(
            json.dumps(state), encoding="utf-8"
        )

    def write_reviewed_head(self, head=None):
        run_dir = self.runs_dir / self.run_id
        run_dir.mkdir(parents=True, exist_ok=True)
        (run_dir / "reviewed-head").write_text(
            (head or self.head) + "\n",
            encoding="utf-8",
        )

    def pr_view_command(self):
        return [
            "gh",
            "pr",
            "view",
            "52",
            "--repo",
            self.repo,
            "--json",
            (
                "number,state,isDraft,baseRefName,headRefName,headRefOid,"
                "mergeStateStatus,mergedAt"
            ),
        ]

    def closure_command(self):
        return [
            "gh",
            "api",
            "graphql",
            "-f",
            f"query={gated_merge.CLOSURE_QUERY}",
            "-F",
            "owner=aram-devdocs",
            "-F",
            "name=sailwind_online",
            "-F",
            "number=52",
        ]

    def closure_result(self, issue=48):
        return {
            "data": {
                "repository": {
                    "pullRequest": {
                        "closingIssuesReferences": {
                            "nodes": [
                                {
                                    "number": issue,
                                    "repository": {
                                        "nameWithOwner": self.repo,
                                    },
                                }
                            ]
                        }
                    }
                }
            }
        }

    def branch_lookup_command(self):
        return [
            "gh",
            "api",
            "graphql",
            "-f",
            f"query={gated_merge.BRANCH_QUERY}",
            "-F",
            "owner=aram-devdocs",
            "-F",
            "name=sailwind_online",
            "-F",
            "qualified=refs/heads/feat/48-gated-self-merge",
        ]

    def branch_result(self, head=None):
        ref = None
        if head is not None:
            ref = {
                "name": "feat/48-gated-self-merge",
                "target": {"oid": head},
            }
        return {
            "data": {
                "repository": {
                    "ref": ref,
                }
            }
        }

    def branch_delete_command(self):
        return [
            "git",
            "push",
            (
                "--force-with-lease=refs/heads/"
                f"feat/48-gated-self-merge:{self.head}"
            ),
            "origin",
            ":refs/heads/feat/48-gated-self-merge",
        ]

    def open_pr(self, **overrides):
        data = {
            "number": 52,
            "state": "OPEN",
            "isDraft": False,
            "baseRefName": "dev",
            "headRefName": "feat/48-gated-self-merge",
            "headRefOid": self.head,
            "mergeStateStatus": "CLEAN",
            "mergedAt": None,
        }
        data.update(overrides)
        return data

    def merged_pr(self):
        return self.open_pr(
            state="MERGED",
            mergeStateStatus="UNKNOWN",
            mergedAt="2026-07-23T12:01:00Z",
        )

    def checks_response(self):
        return (
            [
                "gh",
                "pr",
                "checks",
                "52",
                "--repo",
                self.repo,
                "--required",
                "--json",
                "name,state,bucket",
            ],
            0,
            [{"name": "gate", "state": "SUCCESS", "bucket": "pass"}],
            "",
        )

    def confirmation_pr_response(self):
        return (
            [
                "gh",
                "pr",
                "view",
                "52",
                "--repo",
                self.repo,
                "--json",
                "number,state,mergedAt",
            ],
            0,
            {
                "number": 52,
                "state": "MERGED",
                "mergedAt": "2026-07-23T12:01:00Z",
            },
            "",
        )

    def issue_response(self, state):
        return (
            [
                "gh",
                "issue",
                "view",
                "48",
                "--repo",
                self.repo,
                "--json",
                "number,state",
            ],
            0,
            {"number": 48, "state": state},
            "",
        )

    def success_responses(self):
        return [
            (
                ["gh", "repo", "view", "--json", "nameWithOwner"],
                0,
                {"nameWithOwner": self.repo},
                "",
            ),
            (self.pr_view_command(), 0, self.open_pr(), ""),
            (self.closure_command(), 0, self.closure_result(), ""),
            self.checks_response(),
            (self.pr_view_command(), 0, self.open_pr(), ""),
            (self.closure_command(), 0, self.closure_result(), ""),
            self.checks_response(),
            (
                [
                    "gh",
                    "pr",
                    "merge",
                    "52",
                    "--repo",
                    self.repo,
                    "--squash",
                    "--match-head-commit",
                    self.head,
                ],
                0,
                "",
                "",
            ),
            self.confirmation_pr_response(),
            self.issue_response("CLOSED"),
            (
                self.branch_lookup_command(),
                0,
                self.branch_result(),
                "",
            ),
        ]

    def merged_retry_responses(self, *issue_states, include_branch=True):
        responses = [
            (
                ["gh", "repo", "view", "--json", "nameWithOwner"],
                0,
                {"nameWithOwner": self.repo},
                "",
            ),
            (self.pr_view_command(), 0, self.merged_pr(), ""),
            (self.closure_command(), 0, self.closure_result(), ""),
            self.checks_response(),
            self.confirmation_pr_response(),
            *(self.issue_response(state) for state in issue_states),
        ]
        if include_branch:
            responses.append(
                (
                    self.branch_lookup_command(),
                    0,
                    self.branch_result(),
                    "",
                )
            )
        return responses

    def test_merges_only_after_all_checks_and_confirms_results(self):
        runner = FakeRunner(self.success_responses())

        result = gated_merge.merge_completed_run(
            self.runs_dir, self.run_id, runner=runner
        )

        self.assertEqual(result.pr_number, 52)
        self.assertEqual(result.issue_number, 48)
        self.assertEqual(result.head_oid, self.head)
        self.assertTrue(result.merged)
        merge_call = next(
            call
            for call in runner.calls
            if call[:3] == ["gh", "pr", "merge"]
        )
        self.assertNotIn("--delete-branch", merge_call)
        self.assertFalse((self.runs_dir / "active").exists())
        runner.assert_finished()

    def test_dry_run_checks_every_precondition_without_merging(self):
        responses = self.success_responses()[:7]
        runner = FakeRunner(responses)

        result = gated_merge.merge_completed_run(
            self.runs_dir, self.run_id, runner=runner, dry_run=True
        )

        self.assertFalse(result.merged)
        self.assertNotIn(["gh", "pr", "merge"], [call[:3] for call in runner.calls])
        self.assertTrue((self.runs_dir / "active").exists())
        runner.assert_finished()

    def test_pr_view_uses_supported_fields_and_graphql_checks_issue_link(self):
        runner = FakeRunner(self.success_responses())

        gated_merge.merge_completed_run(
            self.runs_dir, self.run_id, runner=runner
        )

        pr_views = [
            call for call in runner.calls if call[:3] == ["gh", "pr", "view"]
        ]
        self.assertTrue(pr_views)
        self.assertTrue(
            all(
                "closingIssuesReferences" not in call[call.index("--json") + 1]
                for call in pr_views
            )
        )
        self.assertEqual(
            sum(call == self.closure_command() for call in runner.calls),
            2,
        )

    def test_accepts_a_recorded_pr_url_but_uses_its_exact_number(self):
        self.write_state(pr=f"https://github.com/{self.repo}/pull/52")
        runner = FakeRunner(self.success_responses())

        result = gated_merge.merge_completed_run(
            self.runs_dir, self.run_id, runner=runner
        )

        self.assertEqual(result.pr_number, 52)
        runner.assert_finished()

    def test_rejects_incomplete_or_inconsistent_run_state_before_gh(self):
        cases = {
            "phase": {"phase": "wait-ci"},
            "plan": {"plan_open": ""},
            "spec gate": {"gate_spec": "REQUEST-CHANGES"},
            "run id": {"run_id": "49-other-run"},
            "issue": {"issue": "49"},
            "branch": {"branch": "feat/49-other-run"},
            "pr": {"pr": "not-a-pr"},
        }
        for label, overrides in cases.items():
            with self.subTest(label=label):
                self.write_state(**overrides)
                runner = FakeRunner([])
                with self.assertRaises(gated_merge.MergePreconditionError):
                    gated_merge.merge_completed_run(
                        self.runs_dir, self.run_id, runner=runner
                    )
                self.assertEqual(runner.calls, [])

    def test_rejects_missing_or_malformed_reviewed_head_before_gh(self):
        marker = self.runs_dir / self.run_id / "reviewed-head"
        for label, value in {
            "missing": None,
            "malformed": "not-a-commit\n",
        }.items():
            with self.subTest(label=label):
                if value is None:
                    marker.unlink(missing_ok=True)
                else:
                    marker.write_text(value, encoding="utf-8")
                runner = FakeRunner([])
                with self.assertRaises(gated_merge.MergePreconditionError):
                    gated_merge.merge_completed_run(
                        self.runs_dir,
                        self.run_id,
                        runner=runner,
                    )
                self.assertEqual(runner.calls, [])

    def test_rejects_pr_head_that_differs_from_reviewed_head(self):
        responses = self.success_responses()
        responses[1] = (
            self.pr_view_command(),
            0,
            self.open_pr(headRefOid="b" * 40),
            "",
        )
        runner = FakeRunner(responses)

        with self.assertRaisesRegex(
            gated_merge.MergePreconditionError,
            "reviewed head",
        ):
            gated_merge.merge_completed_run(
                self.runs_dir,
                self.run_id,
                runner=runner,
            )

        self.assertFalse(
            any(call[:3] == ["gh", "pr", "merge"] for call in runner.calls)
        )

    def test_rejects_each_remote_pr_mismatch_without_merging(self):
        cases = {
            "number": {"number": 53},
            "state": {"state": "CLOSED"},
            "draft": {"isDraft": True},
            "base": {"baseRefName": "main"},
            "head": {"headRefName": "feat/other"},
            "merge state": {"mergeStateStatus": "BLOCKED"},
            "head oid": {"headRefOid": "invalid"},
        }
        for label, overrides in cases.items():
            with self.subTest(label=label):
                responses = self.success_responses()
                responses[1] = (
                    self.pr_view_command(),
                    0,
                    self.open_pr(**overrides),
                    "",
                )
                runner = FakeRunner(responses)
                with self.assertRaises(gated_merge.MergePreconditionError):
                    gated_merge.merge_completed_run(
                        self.runs_dir, self.run_id, runner=runner
                    )
                self.assertFalse(
                    any(call[:3] == ["gh", "pr", "merge"] for call in runner.calls)
                )

    def test_rejects_empty_pending_failed_or_missing_gate_required_checks(self):
        cases = {
            "empty": [],
            "malformed": ["not-an-object"],
            "pending": [
                {"name": "gate", "state": "PENDING", "bucket": "pending"}
            ],
            "failed": [
                {"name": "gate", "state": "FAILURE", "bucket": "fail"}
            ],
            "missing gate": [
                {"name": "another-check", "state": "SUCCESS", "bucket": "pass"}
            ],
        }
        for label, checks in cases.items():
            with self.subTest(label=label):
                responses = self.success_responses()
                responses[3] = (responses[3][0], 1, checks, "")
                runner = FakeRunner(responses)
                with self.assertRaises(gated_merge.MergePreconditionError):
                    gated_merge.merge_completed_run(
                        self.runs_dir, self.run_id, runner=runner
                    )
                self.assertFalse(
                    any(call[:3] == ["gh", "pr", "merge"] for call in runner.calls)
                )

    def test_rejects_missing_wrong_or_failed_graphql_closure_reference(self):
        cases = {
            "missing": (0, self.closure_result(), ""),
            "wrong issue": (0, self.closure_result(issue=49), ""),
            "command failure": (1, "", "GraphQL unavailable"),
        }
        cases["missing"][1]["data"]["repository"]["pullRequest"][
            "closingIssuesReferences"
        ]["nodes"] = []
        for label, replacement in cases.items():
            with self.subTest(label=label):
                responses = self.success_responses()
                rc, output, error = replacement
                responses[2] = (
                    self.closure_command(),
                    rc,
                    output,
                    error,
                )
                runner = FakeRunner(responses)
                with self.assertRaises(gated_merge.MergePreconditionError):
                    gated_merge.merge_completed_run(
                        self.runs_dir, self.run_id, runner=runner
                    )
                self.assertFalse(
                    any(call[:3] == ["gh", "pr", "merge"] for call in runner.calls)
                )

    def test_rechecks_the_same_head_immediately_before_merge(self):
        responses = self.success_responses()
        responses[4] = (
            self.pr_view_command(),
            0,
            self.open_pr(headRefOid="b" * 40),
            "",
        )
        runner = FakeRunner(responses)

        with self.assertRaisesRegex(
            gated_merge.MergePreconditionError, "head changed"
        ):
            gated_merge.merge_completed_run(
                self.runs_dir, self.run_id, runner=runner
            )

        self.assertFalse(
            any(call[:3] == ["gh", "pr", "merge"] for call in runner.calls)
        )

    def test_rechecks_required_checks_at_the_final_mutation_boundary(self):
        responses = self.success_responses()
        responses[6] = (
            self.checks_response()[0],
            1,
            [{"name": "gate", "state": "FAILURE", "bucket": "fail"}],
            "",
        )
        runner = FakeRunner(responses)

        with self.assertRaisesRegex(
            gated_merge.MergePreconditionError,
            "required checks are not all passed",
        ):
            gated_merge.merge_completed_run(
                self.runs_dir,
                self.run_id,
                runner=runner,
            )

        self.assertEqual(
            sum(
                call == self.checks_response()[0]
                for call in runner.calls
            ),
            2,
        )
        self.assertFalse(
            any(call[:3] == ["gh", "pr", "merge"] for call in runner.calls)
        )

    def test_rejects_failed_merge_or_post_merge_confirmation(self):
        cases = {
            "merge command": (7, 1, "", "merge refused"),
            "pr confirmation": (
                8,
                0,
                {"number": 52, "state": "OPEN", "mergedAt": None},
                "",
            ),
            "issue confirmation": (
                9,
                0,
                {"number": 48, "state": "OPEN"},
                "",
            ),
        }
        for label, replacement in cases.items():
            with self.subTest(label=label):
                responses = self.success_responses()
                index, rc, output, error = replacement
                responses[index] = (responses[index][0], rc, output, error)
                runner = FakeRunner(responses)
                with self.assertRaises(gated_merge.MergeConfirmationError):
                    gated_merge.merge_completed_run(
                        self.runs_dir,
                        self.run_id,
                        runner=runner,
                        confirmation_attempts=1,
                        sleeper=lambda _: None,
                    )
                self.assertTrue((self.runs_dir / "active").exists())

    def test_retry_after_transient_confirmation_failure_is_idempotent(self):
        first_responses = self.success_responses()
        first_responses[8] = (
            self.confirmation_pr_response()[0],
            1,
            "",
            "temporary API failure",
        )
        first_runner = FakeRunner(first_responses)

        with self.assertRaises(gated_merge.MergeConfirmationError):
            gated_merge.merge_completed_run(
                self.runs_dir,
                self.run_id,
                runner=first_runner,
                confirmation_attempts=2,
                sleeper=lambda _: None,
            )
        self.assertTrue((self.runs_dir / "active").exists())

        sleeps = []
        retry_runner = FakeRunner(
            self.merged_retry_responses("OPEN", "CLOSED")
        )
        result = gated_merge.merge_completed_run(
            self.runs_dir,
            self.run_id,
            runner=retry_runner,
            confirmation_attempts=2,
            sleeper=sleeps.append,
        )

        self.assertTrue(result.merged)
        self.assertFalse(
            any(
                call[:3] == ["gh", "pr", "merge"]
                for call in retry_runner.calls
            )
        )
        self.assertEqual(sleeps, [gated_merge.CLOSURE_POLL_INTERVAL_SECONDS])
        self.assertFalse((self.runs_dir / "active").exists())
        retry_runner.assert_finished()

    def test_closure_polling_is_bounded_and_keeps_run_resumable(self):
        runner = FakeRunner(
            self.merged_retry_responses(
                "OPEN",
                "OPEN",
                "OPEN",
                include_branch=False,
            )
        )
        sleeps = []

        with self.assertRaisesRegex(
            gated_merge.MergeConfirmationError,
            "after 3 attempts",
        ):
            gated_merge.merge_completed_run(
                self.runs_dir,
                self.run_id,
                runner=runner,
                confirmation_attempts=3,
                sleeper=sleeps.append,
            )

        self.assertEqual(
            sleeps,
            [
                gated_merge.CLOSURE_POLL_INTERVAL_SECONDS,
                gated_merge.CLOSURE_POLL_INTERVAL_SECONDS,
            ],
        )
        self.assertTrue((self.runs_dir / "active").exists())
        runner.assert_finished()

    def test_retry_repairs_exact_remote_branch_after_delete_failure(self):
        first_responses = self.success_responses()
        first_responses[-1] = (
            self.branch_lookup_command(),
            0,
            self.branch_result(self.head),
            "",
        )
        first_responses.append(
            (
                self.branch_delete_command(),
                1,
                "",
                "temporary delete failure",
            )
        )
        first_responses.append(
            (
                self.branch_lookup_command(),
                0,
                self.branch_result(self.head),
                "",
            )
        )
        first_runner = FakeRunner(first_responses)

        with self.assertRaisesRegex(
            gated_merge.MergeConfirmationError,
            "remote branch deletion failed",
        ):
            gated_merge.merge_completed_run(
                self.runs_dir,
                self.run_id,
                runner=first_runner,
                confirmation_attempts=1,
                sleeper=lambda _: None,
            )
        self.assertTrue((self.runs_dir / "active").exists())

    def test_atomic_delete_rejects_branch_move_between_lookup_and_push(self):
        responses = self.merged_retry_responses("CLOSED")
        responses[-1] = (
            self.branch_lookup_command(),
            0,
            self.branch_result(self.head),
            "",
        )
        responses.extend(
            [
                (
                    self.branch_delete_command(),
                    1,
                    "",
                    "stale info",
                ),
                (
                    self.branch_lookup_command(),
                    0,
                    self.branch_result("b" * 40),
                    "",
                ),
            ]
        )
        runner = FakeRunner(responses)

        with self.assertRaisesRegex(
            gated_merge.MergeConfirmationError,
            "moved to",
        ):
            gated_merge.merge_completed_run(
                self.runs_dir,
                self.run_id,
                runner=runner,
                confirmation_attempts=1,
                sleeper=lambda _: None,
            )

        self.assertIn(self.branch_delete_command(), runner.calls)
        self.assertTrue((self.runs_dir / "active").exists())
        runner.assert_finished()

        retry_responses = self.merged_retry_responses("CLOSED")
        retry_responses[-1] = (
            self.branch_lookup_command(),
            0,
            self.branch_result(self.head),
            "",
        )
        retry_responses.extend(
            [
                (self.branch_delete_command(), 0, "", ""),
                (
                    self.branch_lookup_command(),
                    0,
                    self.branch_result(),
                    "",
                ),
            ]
        )
        retry_runner = FakeRunner(retry_responses)

        result = gated_merge.merge_completed_run(
            self.runs_dir,
            self.run_id,
            runner=retry_runner,
            confirmation_attempts=1,
            sleeper=lambda _: None,
        )

        self.assertTrue(result.merged)
        self.assertFalse(
            any(
                call[:3] == ["gh", "pr", "merge"]
                for call in retry_runner.calls
            )
        )
        self.assertFalse((self.runs_dir / "active").exists())
        retry_runner.assert_finished()

    def test_moved_remote_branch_is_never_deleted(self):
        responses = self.merged_retry_responses("CLOSED")
        responses[-1] = (
            self.branch_lookup_command(),
            0,
            self.branch_result("b" * 40),
            "",
        )
        runner = FakeRunner(responses)

        with self.assertRaisesRegex(
            gated_merge.MergeConfirmationError,
            "moved to",
        ):
            gated_merge.merge_completed_run(
                self.runs_dir,
                self.run_id,
                runner=runner,
                confirmation_attempts=1,
                sleeper=lambda _: None,
            )

        self.assertNotIn(self.branch_delete_command(), runner.calls)
        self.assertTrue((self.runs_dir / "active").exists())
        runner.assert_finished()

    def test_already_absent_remote_branch_completes_handoff(self):
        runner = FakeRunner(self.merged_retry_responses("CLOSED"))

        result = gated_merge.merge_completed_run(
            self.runs_dir,
            self.run_id,
            runner=runner,
            confirmation_attempts=1,
            sleeper=lambda _: None,
        )

        self.assertTrue(result.merged)
        self.assertNotIn(self.branch_delete_command(), runner.calls)
        self.assertFalse((self.runs_dir / "active").exists())
        runner.assert_finished()

    def test_replacement_active_marker_is_preserved_during_clear(self):
        runner = FakeRunner(self.merged_retry_responses("CLOSED"))
        replacement = "49-new-active-run"

        def replace_then_clear(command):
            (self.runs_dir / "active").write_text(
                replacement + "\n",
                encoding="utf-8",
            )
            return gated_merge.run_command(command)

        with self.assertRaisesRegex(
            gated_merge.MergeConfirmationError,
            "active handoff clear failed",
        ):
            gated_merge.merge_completed_run(
                self.runs_dir,
                self.run_id,
                runner=runner,
                state_runner=replace_then_clear,
                confirmation_attempts=1,
                sleeper=lambda _: None,
            )

        self.assertEqual(
            (self.runs_dir / "active").read_text(encoding="utf-8"),
            replacement + "\n",
        )
        runner.assert_finished()

    def test_subprocess_timeout_is_explicit_and_actionable(self):
        command = ["gh", "repo", "view", "--json", "nameWithOwner"]
        with mock.patch.object(
            gated_merge.subprocess,
            "run",
            side_effect=subprocess.TimeoutExpired(
                cmd=command,
                timeout=gated_merge.GH_TIMEOUT_SECONDS,
            ),
        ) as run:
            result = gated_merge.run_command(command)

        self.assertEqual(result.returncode, 124)
        self.assertIn(
            f"timed out after {gated_merge.GH_TIMEOUT_SECONDS} seconds",
            result.stderr,
        )
        run.assert_called_once_with(
            command,
            capture_output=True,
            text=True,
            check=False,
            timeout=gated_merge.GH_TIMEOUT_SECONDS,
        )


if __name__ == "__main__":
    unittest.main()
