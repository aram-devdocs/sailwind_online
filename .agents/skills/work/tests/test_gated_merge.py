import importlib.util
import json
import subprocess
import tempfile
import unittest
from pathlib import Path


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
                "mergeStateStatus"
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

    def open_pr(self, **overrides):
        data = {
            "number": 52,
            "state": "OPEN",
            "isDraft": False,
            "baseRefName": "dev",
            "headRefName": "feat/48-gated-self-merge",
            "headRefOid": self.head,
            "mergeStateStatus": "CLEAN",
        }
        data.update(overrides)
        return data

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
            (
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
            ),
            (self.pr_view_command(), 0, self.open_pr(), ""),
            (self.closure_command(), 0, self.closure_result(), ""),
            (
                [
                    "gh",
                    "pr",
                    "merge",
                    "52",
                    "--repo",
                    self.repo,
                    "--squash",
                    "--delete-branch",
                    "--match-head-commit",
                    self.head,
                ],
                0,
                "",
                "",
            ),
            (
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
            ),
            (
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
                {"number": 48, "state": "CLOSED"},
                "",
            ),
        ]

    def test_merges_only_after_all_checks_and_confirms_results(self):
        runner = FakeRunner(self.success_responses())

        result = gated_merge.merge_completed_run(
            self.runs_dir, self.run_id, runner=runner
        )

        self.assertEqual(result.pr_number, 52)
        self.assertEqual(result.issue_number, 48)
        self.assertEqual(result.head_oid, self.head)
        self.assertTrue(result.merged)
        runner.assert_finished()

    def test_dry_run_checks_every_precondition_without_merging(self):
        responses = self.success_responses()[:6]
        runner = FakeRunner(responses)

        result = gated_merge.merge_completed_run(
            self.runs_dir, self.run_id, runner=runner, dry_run=True
        )

        self.assertFalse(result.merged)
        self.assertNotIn(["gh", "pr", "merge"], [call[:3] for call in runner.calls])
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

    def test_rejects_failed_merge_or_post_merge_confirmation(self):
        cases = {
            "merge command": (6, 1, "", "merge refused"),
            "pr confirmation": (
                7,
                0,
                {"number": 52, "state": "OPEN", "mergedAt": None},
                "",
            ),
            "issue confirmation": (
                8,
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
                        self.runs_dir, self.run_id, runner=runner
                    )


if __name__ == "__main__":
    unittest.main()
