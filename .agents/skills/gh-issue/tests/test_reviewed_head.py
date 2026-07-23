import ast
import contextlib
import importlib.util
import io
import json
import os
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

VALIDATOR = SCRIPT.parents[1] / "validate_skill.py"
VALIDATOR_SPEC = importlib.util.spec_from_file_location(
    "validate_skill",
    VALIDATOR,
)
validate_skill = importlib.util.module_from_spec(VALIDATOR_SPEC)
VALIDATOR_SPEC.loader.exec_module(validate_skill)


class ReviewedHeadTests(unittest.TestCase):
    run_id = "15-ingame-handshake"
    head = "a" * 40
    issue_url = (
        "https://github.com/aram-devdocs/sailwind_online/issues/15"
    )
    original_state_keys = {
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
    }

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.runs_dir = Path(self.temp.name)
        self.run_dir = self.runs_dir / self.run_id
        self.run_dir.mkdir(parents=True)
        self.state_path = self.run_dir / "state.json"
        self.write_state()
        (self.run_dir / "issue-url").write_text(
            self.issue_url + "\n",
            encoding="utf-8",
        )

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

    def test_reviewed_head_rechecks_head_before_marker_write(self):
        before = self.state_path.read_text(encoding="utf-8")
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            run_id=self.run_id,
            head=None,
            no_git=False,
        )
        responses = (
            (0, "", ""),
            (0, f"feat/{self.run_id}", ""),
            (0, self.head, ""),
            (0, "b" * 40, ""),
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
                side_effect=responses,
            ) as run,
            self.assertRaisesRegex(SystemExit, "review head changed"),
        ):
            gh_issue_run.cmd_record_reviewed_head(args)

        self.assertEqual(run.call_count, 4)
        self.assertEqual(
            self.state_path.read_text(encoding="utf-8"),
            before,
        )
        self.assertFalse((self.run_dir / "reviewed-head").exists())

    def test_init_run_preserves_exact_state_schema_and_records_marker(self):
        run_id = "16-explicit-repository"
        issue_url = (
            "https://github.com/aram-devdocs/sailwind_online/issues/16"
        )

        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--runs-dir",
                str(self.runs_dir),
                "init-run",
                "--issue",
                "16",
                "--slug",
                "explicit-repository",
                "--issue-url",
                issue_url,
                "--no-git",
            ],
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        state = json.loads(
            (self.runs_dir / run_id / "state.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(set(state), self.original_state_keys)
        self.assertNotIn("issue_url", state)
        self.assertEqual(
            (self.runs_dir / run_id / "issue-url").read_text(
                encoding="utf-8"
            ),
            issue_url + "\n",
        )

    def test_init_run_refuses_missing_issue_url(self):
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--runs-dir",
                str(self.runs_dir),
                "init-run",
                "--issue",
                "16",
                "--slug",
                "missing-repository",
                "--no-git",
            ],
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--issue-url", result.stderr)
        self.assertFalse(
            (self.runs_dir / "16-missing-repository" / "state.json").exists()
        )

    def test_resume_rejects_corrupt_identity_before_git_or_path_mutation(self):
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            issue="15",
            slug="ingame-handshake",
            issue_url=self.issue_url,
            resume=True,
            no_git=False,
        )
        outside = self.runs_dir.parent / "outside-worktree"
        corruptions = (
            ("branch", "feat/99-unrelated"),
            ("worktree", "../outside-worktree"),
            ("worktree", str(outside.resolve())),
            ("worktree", ".worktrees/../outside-worktree"),
        )
        for key, value in corruptions:
            with self.subTest(key=key, value=value):
                self.write_state()
                state = json.loads(self.state_path.read_text(encoding="utf-8"))
                state[key] = value
                corrupted = json.dumps(state)
                self.state_path.write_text(corrupted, encoding="utf-8")
                (self.runs_dir / "active").unlink(missing_ok=True)

                with (
                    mock.patch.object(
                        gh_issue_run,
                        "repo_root",
                        return_value=self.runs_dir,
                    ),
                    mock.patch.object(gh_issue_run, "run_cmd") as run,
                    self.assertRaisesRegex(SystemExit, "identity"),
                ):
                    gh_issue_run.cmd_init_run(args)

                run.assert_not_called()
                self.assertEqual(
                    self.state_path.read_text(encoding="utf-8"),
                    corrupted,
                )
                self.assertFalse((self.runs_dir / "active").exists())
                self.assertFalse(outside.exists())

    def test_update_state_cannot_corrupt_persisted_run_identity(self):
        before = self.state_path.read_text(encoding="utf-8")
        for key, value in (
            ("branch", "feat/99-unrelated"),
            ("worktree", "../outside-worktree"),
            ("run_id", "99-unrelated"),
        ):
            args = SimpleNamespace(
                runs_dir=str(self.runs_dir),
                run_id=self.run_id,
                key=key,
                value=value,
            )
            with (
                self.subTest(key=key),
                mock.patch.object(
                    gh_issue_run,
                    "repo_root",
                    return_value=self.runs_dir,
                ),
                self.assertRaisesRegex(SystemExit, "identity"),
            ):
                gh_issue_run.cmd_update_state(args)
            self.assertEqual(
                self.state_path.read_text(encoding="utf-8"),
                before,
            )

    def test_issue_url_must_match_tracked_repository(self):
        with self.assertRaisesRegex(SystemExit, "tracked repository"):
            gh_issue_run.parse_issue_url(
                "https://github.com/attacker/unrelated/issues/15",
                "15",
            )

    def test_repository_config_missing_or_malformed_fails_closed(self):
        issue_url = (
            "https://github.com/aram-devdocs/sailwind_online/issues/15"
        )
        for label, contents in (
            ("missing", None),
            ("invalid JSON", "{"),
            ("invalid shape", '{"repository": 3}'),
            (
                "extra field",
                '{"repository":"aram-devdocs/sailwind_online","other":"x"}',
            ),
        ):
            with self.subTest(label=label):
                config = self.runs_dir / f"{label}.json"
                if contents is not None:
                    config.write_text(contents, encoding="utf-8")
                with (
                    mock.patch.object(
                        gh_issue_run,
                        "REPOSITORY_CONFIG",
                        config,
                    ),
                    self.assertRaisesRegex(
                        SystemExit,
                        "tracked repository identity",
                    ),
                ):
                    gh_issue_run.parse_issue_url(issue_url, "15")

    def test_repo_root_probe_is_bounded_and_timeout_falls_back(self):
        fallback = self.runs_dir / "fallback"
        (fallback / ".agents").mkdir(parents=True)
        with (
            mock.patch.object(
                gh_issue_run.subprocess,
                "run",
                side_effect=subprocess.TimeoutExpired(
                    ["git", "rev-parse"],
                    5,
                ),
            ) as run,
            mock.patch.object(Path, "cwd", return_value=fallback),
        ):
            self.assertEqual(gh_issue_run.repo_root(), fallback)

        self.assertEqual(
            run.call_args.kwargs["timeout"],
            gh_issue_run.PROBE_TIMEOUT_SECONDS,
        )

    def test_validator_probes_are_bounded_and_timeout_is_reported(self):
        with mock.patch.object(
            validate_skill.subprocess,
            "run",
            side_effect=subprocess.TimeoutExpired(["python", "--version"], 5),
        ) as run:
            self.assertEqual(validate_skill.python_exe(), sys.executable)
        self.assertTrue(run.call_args_list)
        for call in run.call_args_list:
            self.assertEqual(
                call.kwargs["timeout"],
                validate_skill.PROBE_TIMEOUT_SECONDS,
            )

        output = io.StringIO()
        with (
            mock.patch.object(
                validate_skill,
                "python_exe",
                return_value=sys.executable,
            ),
            mock.patch.object(
                validate_skill.subprocess,
                "run",
                side_effect=subprocess.TimeoutExpired(
                    [sys.executable, str(SCRIPT), "--help"],
                    5,
                ),
            ) as help_run,
            contextlib.redirect_stdout(output),
        ):
            self.assertEqual(validate_skill.main(), 1)
        self.assertEqual(
            help_run.call_args.kwargs["timeout"],
            validate_skill.PROBE_TIMEOUT_SECONDS,
        )
        self.assertIn("timed out", output.getvalue())

    def test_production_and_validator_subprocesses_have_timeouts(self):
        scripts = (
            SCRIPT,
            VALIDATOR,
            SCRIPT.parents[2] / "work" / "scripts" / "gated_merge.py",
        )
        for script in scripts:
            tree = ast.parse(script.read_text(encoding="utf-8"))
            for node in ast.walk(tree):
                if not isinstance(node, ast.Call):
                    continue
                function = node.func
                if not (
                    isinstance(function, ast.Attribute)
                    and function.attr == "run"
                    and isinstance(function.value, ast.Name)
                    and function.value.id == "subprocess"
                ):
                    continue
                self.assertIn(
                    "timeout",
                    {keyword.arg for keyword in node.keywords},
                    f"unbounded subprocess.run in {script}:{node.lineno}",
                )

    def test_live_lock_cannot_be_stolen_or_released_by_another_owner(self):
        first_dir = self.runs_dir / "first-lock"
        other_dir = self.runs_dir / "other-lock"
        first = gh_issue_run.acquire_lock(first_dir)
        other = gh_issue_run.acquire_lock(other_dir)
        try:
            old = 1
            os.utime(first_dir / ".lock", (old, old))
            gh_issue_run.release_lock(other)
            other = None
            with (
                mock.patch.object(
                    gh_issue_run,
                    "LOCK_WAIT_SECONDS",
                    0,
                ),
                self.assertRaisesRegex(SystemExit, "another writer is active"),
            ):
                gh_issue_run.acquire_lock(first_dir)
        finally:
            if other is not None:
                gh_issue_run.release_lock(other)
            gh_issue_run.release_lock(first)

        replacement = gh_issue_run.acquire_lock(first_dir)
        gh_issue_run.release_lock(replacement)

    def test_lock_deadline_uses_monotonic_clock_during_wall_rollback(self):
        owner = gh_issue_run.acquire_lock(self.runs_dir / "clock-lock")
        ticks = iter((10.0, 10.0, 11.0))
        wall_clock = mock.Mock(side_effect=(1000.0, -1000.0))
        try:
            with (
                mock.patch.object(
                    gh_issue_run,
                    "LOCK_WAIT_SECONDS",
                    1,
                ),
                mock.patch.object(gh_issue_run.time, "time", wall_clock),
                self.assertRaisesRegex(SystemExit, "another writer is active"),
            ):
                gh_issue_run.acquire_lock(
                    self.runs_dir / "clock-lock",
                    clock=lambda: next(ticks),
                    sleeper=lambda _: None,
                )
        finally:
            gh_issue_run.release_lock(owner)
        wall_clock.assert_not_called()

    def test_run_paths_reject_traversal_and_absolute_ids_before_io(self):
        args = SimpleNamespace(runs_dir=str(self.runs_dir))
        outside = self.runs_dir.parent / (
            self.runs_dir.name + "-outside-run"
        )
        invalid_ids = (
            "../outside-run",
            r"..\outside-run",
            str(outside.resolve()),
            "/absolute-run",
            "15-dot.",
            "15-nested/run",
        )
        helpers = (
            gh_issue_run.run_dir,
            gh_issue_run.state_path,
            gh_issue_run.reviewed_head_path,
            gh_issue_run.issue_url_path,
        )
        for run_id in invalid_ids:
            for helper in helpers:
                with (
                    self.subTest(run_id=run_id, helper=helper.__name__),
                    self.assertRaisesRegex(SystemExit, "invalid run id"),
                ):
                    helper(args, run_id)

            migrate_args = SimpleNamespace(
                runs_dir=str(self.runs_dir),
                run_id=run_id,
                issue_url=self.issue_url,
            )
            with (
                self.subTest(run_id=run_id, command="migrate"),
                mock.patch.object(
                    gh_issue_run,
                    "acquire_lock",
                    side_effect=AssertionError("lock attempted"),
                ) as acquire,
                self.assertRaisesRegex(SystemExit, "invalid run id"),
            ):
                gh_issue_run.cmd_migrate_issue_url(migrate_args)
            acquire.assert_not_called()
            self.assertFalse(outside.exists())

    def test_migrates_completed_legacy_run_from_explicit_issue_url(self):
        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        state["issue_url"] = self.issue_url
        state["phase"] = "done"
        self.state_path.write_text(json.dumps(state), encoding="utf-8")
        (self.run_dir / "issue-url").unlink()

        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--runs-dir",
                str(self.runs_dir),
                "migrate-issue-url",
                "--run-id",
                self.run_id,
                "--issue-url",
                self.issue_url,
            ],
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)
        migrated = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertEqual(set(migrated), self.original_state_keys)
        self.assertNotIn("issue_url", migrated)
        self.assertEqual(
            (self.run_dir / "issue-url").read_text(encoding="utf-8"),
            self.issue_url + "\n",
        )
        self.assertTrue(self.state_path.with_suffix(".json.bak").exists())

    def test_migrates_active_legacy_run_after_clean_identity_check(self):
        (self.run_dir / "issue-url").unlink()
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )
        worktree = self.runs_dir / ".worktrees" / self.run_id
        worktree.mkdir(parents=True)
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            run_id=self.run_id,
            issue_url=(
                "https://github.com/aram-devdocs/sailwind_online/issues/15"
            ),
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
                    (0, "", ""),
                    (0, "feat/15-ingame-handshake", ""),
                ],
            ) as run,
        ):
            gh_issue_run.cmd_migrate_issue_url(args)

        self.assertEqual(
            [call.args[0] for call in run.call_args_list],
            [
                ["git", "status", "--porcelain"],
                ["git", "branch", "--show-current"],
            ],
        )
        migrated = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertEqual(set(migrated), self.original_state_keys)
        self.assertEqual(
            (self.run_dir / "issue-url").read_text(encoding="utf-8"),
            args.issue_url + "\n",
        )

    def test_active_legacy_migration_refuses_dirty_worktree(self):
        (self.run_dir / "issue-url").unlink()
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )
        worktree = self.runs_dir / ".worktrees" / self.run_id
        worktree.mkdir(parents=True)
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            run_id=self.run_id,
            issue_url=(
                "https://github.com/aram-devdocs/sailwind_online/issues/15"
            ),
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
                return_value=(0, "M unrelated.txt", ""),
            ),
            self.assertRaisesRegex(SystemExit, "must be clean"),
        ):
            gh_issue_run.cmd_migrate_issue_url(args)

        unchanged = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertNotIn("issue_url", unchanged)
        self.assertFalse((self.run_dir / "issue-url").exists())

    def test_issue_url_marker_is_immutable_and_config_bound(self):
        marker = self.run_dir / "issue-url"
        with self.assertRaisesRegex(SystemExit, "immutable"):
            gh_issue_run.write_issue_url_marker(
                marker,
                self.issue_url + "/replacement",
            )
        self.assertEqual(
            marker.read_text(encoding="utf-8"),
            self.issue_url + "\n",
        )

        marker.write_text(
            "https://github.com/attacker/unrelated/issues/15\n",
            encoding="utf-8",
        )
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            run_id=self.run_id,
            issue_url=self.issue_url,
        )
        with self.assertRaisesRegex(SystemExit, "tracked repository"):
            gh_issue_run.cmd_migrate_issue_url(args)
        self.assertEqual(
            marker.read_text(encoding="utf-8"),
            "https://github.com/attacker/unrelated/issues/15\n",
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
        (self.run_dir / "reviewed-head").write_text(
            self.head + "\n",
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
                    (0, "", ""),
                    (1, "", "worktree is locked"),
                ],
            ) as run,
            self.assertRaisesRegex(SystemExit, "worktree removal failed"),
        ):
            gh_issue_run.cmd_cleanup_worktree(args)

        self.assertEqual(
            run.call_args_list[2].args[0],
            ["git", "worktree", "remove", str(worktree)],
        )
        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertEqual(state["phase"], "review")
        self.assertEqual(
            (self.runs_dir / "active").read_text(encoding="utf-8"),
            self.run_id + "\n",
        )

    def test_cleanup_rejects_post_review_dirty_or_untracked_files(self):
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )
        (self.run_dir / "reviewed-head").write_text(
            self.head + "\n",
            encoding="utf-8",
        )
        worktree = self.runs_dir / ".worktrees" / self.run_id
        worktree.mkdir(parents=True)
        registry = (
            f"worktree {worktree}\n"
            f"HEAD {self.head}\n"
            "branch refs/heads/feat/15-ingame-handshake\n"
        )
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            run_id=self.run_id,
            no_git=False,
        )

        for label, status in (
            ("tracked", " M reviewed-file.txt"),
            ("untracked", "?? post-review.txt"),
        ):
            with (
                self.subTest(label=label),
                mock.patch.object(
                    gh_issue_run,
                    "repo_root",
                    return_value=self.runs_dir,
                ),
                mock.patch.object(
                    gh_issue_run,
                    "run_cmd",
                    side_effect=[
                        (0, registry, ""),
                        (0, status, ""),
                    ],
                ) as run,
                self.assertRaisesRegex(SystemExit, "worktree is not clean"),
            ):
                gh_issue_run.cmd_cleanup_worktree(args)

            self.assertEqual(len(run.call_args_list), 2)
            self.assertEqual(
                run.call_args_list[1].args[0],
                [
                    "git",
                    "status",
                    "--porcelain",
                    "--untracked-files=all",
                ],
            )
            state = json.loads(self.state_path.read_text(encoding="utf-8"))
            self.assertEqual(state["phase"], "review")
            self.assertEqual(
                (self.runs_dir / "active").read_text(encoding="utf-8"),
                self.run_id + "\n",
            )

    def test_cleanup_removes_exact_stale_registration_before_done(self):
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )
        (self.run_dir / "reviewed-head").write_text(
            self.head + "\n",
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
        (self.run_dir / "reviewed-head").write_text(
            self.head + "\n",
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

    def test_cleanup_rejects_replacement_worktree_identity(self):
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )
        (self.run_dir / "reviewed-head").write_text(
            self.head + "\n",
            encoding="utf-8",
        )
        worktree = self.runs_dir / ".worktrees" / self.run_id
        replacement = (
            f"worktree {worktree}\n"
            f"HEAD {'b' * 40}\n"
            "branch refs/heads/feat/99-replacement\n"
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
                return_value=(0, replacement, ""),
            ) as run,
            self.assertRaisesRegex(SystemExit, "worktree identity mismatch"),
        ):
            gh_issue_run.cmd_cleanup_worktree(args)

        self.assertEqual(len(run.call_args_list), 1)
        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertEqual(state["phase"], "review")
        self.assertEqual(
            (self.runs_dir / "active").read_text(encoding="utf-8"),
            self.run_id + "\n",
        )

    def test_cleanup_excludes_recreation_through_done_write(self):
        (self.runs_dir / "active").write_text(
            self.run_id + "\n",
            encoding="utf-8",
        )
        (self.run_dir / "reviewed-head").write_text(
            self.head + "\n",
            encoding="utf-8",
        )
        worktree = self.runs_dir / ".worktrees" / self.run_id
        worktree.mkdir(parents=True)
        registry = (
            f"worktree {worktree}\n"
            f"HEAD {self.head}\n"
            "branch refs/heads/feat/15-ingame-handshake\n"
        )
        checkpoints = []
        args = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            run_id=self.run_id,
            no_git=False,
        )
        original_write_state = gh_issue_run.write_state
        registry_reads = 0

        def inspect_registry(command, cwd=None):
            nonlocal registry_reads
            if command == ["git", "worktree", "list", "--porcelain"]:
                registry_reads += 1
                return (0, registry, "") if registry_reads == 1 else (0, "", "")
            if command[:2] == ["git", "status"]:
                return 0, "", ""
            if command[:3] == ["git", "worktree", "remove"]:
                worktree.rmdir()
                return 0, "", ""
            raise AssertionError(f"unexpected command: {command}")

        competing = SimpleNamespace(
            runs_dir=str(self.runs_dir),
            issue="15",
            slug="ingame-handshake",
            issue_url=self.issue_url,
            resume=True,
            no_git=False,
        )

        def attempt_recreation():
            checkpoints.append("after-remove")
            self.assertFalse(worktree.exists())
            with self.assertRaisesRegex(
                SystemExit,
                "another writer is active",
            ):
                gh_issue_run.cmd_init_run(competing)
            self.assertFalse(worktree.exists())

        def assert_locked_write(path, data):
            checkpoints.append("done-write")
            with self.assertRaisesRegex(
                SystemExit,
                "another writer is active",
            ):
                gh_issue_run.acquire_lock(self.runs_dir)
            return original_write_state(path, data)

        args._after_remove_hook = attempt_recreation
        with (
            mock.patch.object(
                gh_issue_run,
                "repo_root",
                return_value=self.runs_dir,
            ),
            mock.patch.object(
                gh_issue_run,
                "run_cmd",
                side_effect=inspect_registry,
            ),
            mock.patch.object(
                gh_issue_run,
                "write_state",
                side_effect=assert_locked_write,
            ),
            mock.patch.object(gh_issue_run, "LOCK_WAIT_SECONDS", 0),
        ):
            gh_issue_run.cmd_cleanup_worktree(args)

        self.assertEqual(registry_reads, 2)
        self.assertEqual(checkpoints, ["after-remove", "done-write"])
        self.assertFalse(worktree.exists())
        replacement = gh_issue_run.acquire_lock(self.runs_dir)
        gh_issue_run.release_lock(replacement)
        state = json.loads(self.state_path.read_text(encoding="utf-8"))
        self.assertEqual(state["phase"], "done")

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
