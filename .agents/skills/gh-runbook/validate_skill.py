#!/usr/bin/env python
"""Self-check for the gh-runbook skill.

Confirms the SKILL.md frontmatter and required sections, every referenced file
(the creator script, the example manifest, the workflow contract), and that the
creator script is dry-run by default (guards its create path behind --apply).
Exits nonzero on any problem.

Run: python validate_skill.py  (python3 also works).
"""
import re
import sys
from pathlib import Path

SKILL_DIR = Path(__file__).resolve().parent
SKILL_NAME = "gh-runbook"

REQUIRED_FRONTMATTER = ["name", "description"]
REQUIRED_SECTIONS = [
    "## Purpose",
    "## When to use",
    "## Process",
    "## The manifest",
    "## Creating the issues",
    "## Anti-patterns",
]
REQUIRED_FILES = [
    "references/workflow-contract.md",
    "references/example-manifest.md",
    "scripts/create-issues.sh",
]


def frontmatter(text):
    m = re.match(r"^---\n(.*?)\n---\n", text, re.S)
    return m.group(1) if m else None


def main():
    errors = []
    skill_md = SKILL_DIR / "SKILL.md"
    if not skill_md.is_file():
        print(f"FAIL: {SKILL_NAME}: SKILL.md missing")
        return 1

    text = skill_md.read_text(encoding="utf-8")

    front = frontmatter(text)
    if front is None:
        errors.append("frontmatter block (--- ... ---) missing")
        front = ""

    for key in REQUIRED_FRONTMATTER:
        if not re.search(rf"^{key}\s*:", front, re.M):
            errors.append(f"frontmatter key '{key}' missing")

    nm = re.search(r"^name\s*:\s*(\S+)", front, re.M)
    if nm and nm.group(1) != SKILL_NAME:
        errors.append(f"frontmatter name '{nm.group(1)}' does not match dir '{SKILL_NAME}'")

    for sec in REQUIRED_SECTIONS:
        if sec not in text:
            errors.append(f"required section '{sec}' missing")

    for rel in REQUIRED_FILES:
        if not (SKILL_DIR / rel).is_file():
            errors.append(f"referenced file '{rel}' missing")

    if "—" in text:
        errors.append("em dash present in SKILL.md (banned by documentation rule)")

    # The creator must not create without --apply. Confirm the apply gate exists
    # and that the create call is guarded by it, because a script that writes to
    # the board on a bare run defeats the dry-run contract.
    script = SKILL_DIR / "scripts" / "create-issues.sh"
    if script.is_file():
        src = script.read_text(encoding="utf-8")
        if "--apply" not in src:
            errors.append("create-issues.sh does not mention --apply")
        if 'apply=0' not in src:
            errors.append("create-issues.sh does not default apply to 0 (dry-run)")
        if 'gh "${args[@]}"' not in src:
            errors.append("create-issues.sh has no guarded gh create call")

    if errors:
        print(f"FAIL: {SKILL_NAME}")
        for e in errors:
            print(f"  - {e}")
        return 1

    print(f"PASS: {SKILL_NAME} skill structure valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
