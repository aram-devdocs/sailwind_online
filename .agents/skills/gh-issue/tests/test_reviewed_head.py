import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "gh_issue_run.py"
SPEC = importlib.util.spec_from_file_location("gh_issue_run", SCRIPT)
gh_issue_run = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(gh_issue_run)


class ReviewedHeadTests(unittest.TestCase):
    run_id = "15-ingame-handshake"
    head = "a" * 40

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.runs_dir = Path(self.temp.name)
        self.run_dir = self.runs_dir / self.run_id
        self.run_dir.mkdir(parents=True)
        self.state_path = self.run_dir / "state.json"
        self.write_state()

    def tearDown(self):
        self.temp.cleanup()

    def write_state(self):
        self.state_path.write_text(
            json.dumps(
                {
                    "run_id": self.run_id,
                    "issue": "15",
                    "phase": "review",
                    "branch": "feat/15-ingame-handshake",
                    "worktree": ".worktrees/15-ingame-handshake",
                    "pr": "",
                    "gate_spec": "APPROVE",
                    "gate_quality": "APPROVE",
                    "gate_architecture": "APPROVE",
                    "gate_security": "APPROVE",
                    "plan_open": "0",
                    "updated_at": "2026-07-23T12:00:00Z",
                }
            ),
            encoding="utf-8",
        )

    def run_command(self, *args):
        return subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--runs-dir",
                str(self.runs_dir),
                "record-reviewed-head",
                "--run-id",
                self.run_id,
                *args,
            ],
            capture_output=True,
            text=True,
            check=False,
        )

    def test_records_reviewed_head_and_clears_every_prior_verdict(self):
        result = self.run_command(
            "--no-git",
            "--head",
            self.head,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.run_dir / "reviewed-head").read_text(encoding="utf-8"),
            self.head + "\n",
        )
        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        for gate in ("spec", "quality", "architecture", "security"):
            self.assertEqual(state[f"gate_{gate}"], "")
        self.assertTrue(self.state_path.with_suffix(".json.bak").exists())

    def test_replacing_reviewed_head_invalidates_existing_verdicts(self):
        old_head = "b" * 40
        (self.run_dir / "reviewed-head").write_text(
            old_head + "\n",
            encoding="utf-8",
        )

        result = self.run_command(
            "--no-git",
            "--head",
            self.head,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            (self.run_dir / "reviewed-head").read_text(encoding="utf-8"),
            self.head + "\n",
        )
        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertTrue(
            all(
                state[f"gate_{gate}"] == ""
                for gate in ("spec", "quality", "architecture", "security")
            )
        )

    def test_invalid_test_head_fails_without_changing_state(self):
        before = self.state_path.read_text(encoding="utf-8")

        result = self.run_command(
            "--no-git",
            "--head",
            "not-a-head",
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("40-character", result.stderr)
        self.assertEqual(
            self.state_path.read_text(encoding="utf-8"),
            before,
        )
        self.assertFalse((self.run_dir / "reviewed-head").exists())

    def test_head_override_requires_no_git_test_seam(self):
        result = self.run_command("--head", self.head)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--head requires --no-git", result.stderr)
        self.assertFalse((self.run_dir / "reviewed-head").exists())

    def test_cleanup_keeps_completed_run_active_for_merge_resume(self):
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )

        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--runs-dir",
                str(self.runs_dir),
                "cleanup-worktree",
                "--run-id",
                self.run_id,
                "--no-git",
            ],
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertEqual(state["phase"], "done")
        self.assertEqual(
            (self.runs_dir / "active").read_text(encoding="utf-8"),
            self.run_id + "\n",
        )

        resume = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--runs-dir",
                str(self.runs_dir),
                "validate-resume",
            ],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(resume.returncode, 0, resume.stderr)
        self.assertIn("resume the /work gated merge", resume.stdout)
        self.assertNotIn("recreate it", resume.stdout)

    def test_cleanup_failure_keeps_run_non_done_and_active(self):
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )
        worktree = (
            self.runs_dir
            / ".worktrees"
            / self.run_id
        )
        worktree.mkdir(parents=True)
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            run_id=self.run_id,
            no_git=False,
        )

        registered = (
            f"worktree {worktree}\n"
            f"HEAD {self.head}\n"
            "branch refs/heads/feat/15-ingame-handshake\n"
        )
        with (
            mock.patch.object(
                gh_issue_run,
                "repo_root",
                return_value=self.runs_dir,
            ),
            mock.patch.object(
                gh_issue_run,
                "run_cmd",
                side_effect=[
                    (0, registered, ""),
                    (1, "", "worktree is locked"),
                ],
            ),
            self.assertRaisesRegex(SystemExit, "worktree removal failed"),
        ):
            gh_issue_run.cmd_cleanup_worktree(args)

        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertEqual(state["phase"], "review")
        self.assertEqual(
            (self.runs_dir / "active").read_text(encoding="utf-8"),
            self.run_id + "\n",
        )

    def test_cleanup_prunes_stale_registered_worktree_before_done(self):
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )
        worktree = self.runs_dir / ".worktrees" / self.run_id
        registered = (
            f"worktree {worktree}\n"
            f"HEAD {self.head}\n"
            "branch refs/heads/feat/15-ingame-handshake\n"
        )
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            run_id=self.run_id,
            no_git=False,
        )

        with (
            mock.patch.object(
                gh_issue_run,
                "repo_root",
                return_value=self.runs_dir,
            ),
            mock.patch.object(
                gh_issue_run,
                "run_cmd",
                side_effect=[
                    (0, registered, ""),
                    (1, "", "not a working tree"),
                    (0, "", ""),
                    (0, "", ""),
                ],
            ) as run,
        ):
            gh_issue_run.cmd_cleanup_worktree(args)

        self.assertEqual(
            [call.args[0] for call in run.call_args_list],
            [
                ["git", "worktree", "list", "--porcelain"],
                [
                    "git",
                    "worktree",
                    "remove",
                    str(worktree),
                    "--force",
                ],
                ["git", "worktree", "prune", "--expire", "now"],
                ["git", "worktree", "list", "--porcelain"],
            ],
        )
        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertEqual(state["phase"], "done")
        self.assertEqual(
            (self.runs_dir / "active").read_text(encoding="utf-8"),
            self.run_id + "\n",
        )

    def test_cleanup_keeps_run_non_done_when_registration_remains(self):
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )
        worktree = self.runs_dir / ".worktrees" / self.run_id
        registered = (
            f"worktree {worktree}\n"
            f"HEAD {self.head}\n"
            "branch refs/heads/feat/15-ingame-handshake\n"
        )
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            run_id=self.run_id,
            no_git=False,
        )

        with (
            mock.patch.object(
                gh_issue_run,
                "repo_root",
                return_value=self.runs_dir,
            ),
            mock.patch.object(
                gh_issue_run,
                "run_cmd",
                side_effect=[
                    (0, registered, ""),
                    (1, "", "not a working tree"),
                    (0, "", ""),
                    (0, registered, ""),
                ],
            ),
            self.assertRaisesRegex(SystemExit, "remains registered"),
        ):
            gh_issue_run.cmd_cleanup_worktree(args)

        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertEqual(state["phase"], "review")
        self.assertEqual(
            (self.runs_dir / "active").read_text(encoding="utf-8"),
            self.run_id + "\n",
        )

    def test_clear_active_preserves_replacement_marker(self):
        replacement = "99-replacement-run"
        (self.runs_dir / "active").write_text(
            replacement + "\n",
            encoding="utf-8",
        )

        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--runs-dir",
                str(self.runs_dir),
                "clear-active",
                "--expected-run-id",
                self.run_id,
            ],
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("active run changed", result.stderr)
        self.assertEqual(
            (self.runs_dir / "active").read_text(encoding="utf-8"),
            replacement + "\n",
        )


if __name__ == "__main__":
    unittest.main()
