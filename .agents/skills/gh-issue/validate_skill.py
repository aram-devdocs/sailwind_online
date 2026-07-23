#!/usr/bin/env python3
"""Self-check for the gh-issue skill directory.

Exits nonzero on any problem so a run can gate on skill integrity. Checks:
  1. SKILL.md exists with YAML frontmatter carrying name, description.
  2. references/workflow-contract.md exists.
  3. scripts/gh_issue_run.py exists and exposes the documented subcommands
     (probed by parsing `--help` output, so the check stays in sync with the
     real argparse surface).
  4. The state.json flat-key contract is documented in the module docstring
     and in the workflow contract.
  5. Required SKILL.md sections are present.

Stdlib only. Tries python3 then python for the subcommand probe so the check is
portable; the dev path on this Windows machine is `python`.
"""

import re
import subprocess
import sys
from pathlib import Path

SKILL_DIR = Path(__file__).resolve().parent

# Subcommands the docs promise and callers rely on.
REQUIRED_SUBCOMMANDS = (
    "init-run",
    "update-state",
    "get-state",
    "set-active",
    "clear-active",
    "record-reviewed-head",
    "validate-resume",
    "poll-pr",
    "cleanup-worktree",
)

# Required top-level sections in SKILL.md (markdown headings).
REQUIRED_SECTIONS = (
    "Purpose",
    "Phase lifecycle",
    "State transitions",
    "Worktree isolation",
    "Review gates",
    "Resume",
)

# Flat keys the contract must document.
REQUIRED_KEYS = (
    "run_id", "issue", "phase", "branch", "worktree", "pr",
    "gate_spec", "gate_quality", "gate_architecture", "gate_security",
    "plan_open", "updated_at",
)


def fail(problems):
    print("gh-issue skill validation: FAIL")
    for pb in problems:
        print(f"  - {pb}")
    return 1


def python_exe():
    """Return the first working interpreter: python3 then python."""
    for exe in ("python3", "python"):
        try:
            r = subprocess.run([exe, "--version"], capture_output=True, text=True)
            if r.returncode == 0:
                return exe
        except (OSError, subprocess.SubprocessError):
            continue
    return sys.executable


def main():
    problems = []

    skill_md = SKILL_DIR / "SKILL.md"
    contract = SKILL_DIR / "references" / "workflow-contract.md"
    script = SKILL_DIR / "scripts" / "gh_issue_run.py"

    # 1. SKILL.md + frontmatter.
    if not skill_md.is_file():
        problems.append("SKILL.md is missing")
        skill_text = ""
    else:
        skill_text = skill_md.read_text(encoding="utf-8")
        fm = re.match(r"^---\s*\n(.*?)\n---\s*\n", skill_text, re.DOTALL)
        if not fm:
            problems.append("SKILL.md has no YAML frontmatter block")
        else:
            front = fm.group(1)
            for field in ("name:", "description:"):
                if field not in front:
                    problems.append(f"SKILL.md frontmatter missing '{field}'")
            if "name: gh-issue" not in front:
                problems.append("SKILL.md frontmatter name is not 'gh-issue'")

    # 2. references/workflow-contract.md.
    if not contract.is_file():
        problems.append("references/workflow-contract.md is missing")
        contract_text = ""
    else:
        contract_text = contract.read_text(encoding="utf-8")

    # 3. scripts/gh_issue_run.py + subcommand surface.
    if not script.is_file():
        problems.append("scripts/gh_issue_run.py is missing")
    else:
        exe = python_exe()
        r = subprocess.run(
            [exe, str(script), "--help"], capture_output=True, text=True
        )
        if r.returncode != 0:
            problems.append(f"'gh_issue_run.py --help' exited {r.returncode}")
        help_text = r.stdout + r.stderr
        for cmd in REQUIRED_SUBCOMMANDS:
            if cmd not in help_text:
                problems.append(f"subcommand '{cmd}' absent from --help output")

    # 4. Flat-key contract documented (module docstring + workflow contract).
    script_text = script.read_text(encoding="utf-8") if script.is_file() else ""
    for key in REQUIRED_KEYS:
        if key not in script_text:
            problems.append(f"flat key '{key}' not documented in gh_issue_run.py")
        if contract_text and key not in contract_text:
            problems.append(f"flat key '{key}' not documented in workflow-contract.md")
    if "flat" not in script_text.lower():
        problems.append("gh_issue_run.py docstring does not state the flat-key contract")
    if contract_text and "flat" not in contract_text.lower():
        problems.append("workflow-contract.md does not state the flat-key contract")

    # 5. Required SKILL.md sections.
    for sec in REQUIRED_SECTIONS:
        if skill_text and sec.lower() not in skill_text.lower():
            problems.append(f"SKILL.md missing a '{sec}' section")

    if problems:
        return fail(problems)
    print("gh-issue skill validation: PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
