import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "gh_issue_run.py"


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


if __name__ == "__main__":
    unittest.main()
