#!/usr/bin/env python
"""Self-check for the subagent-driven-development skill.

Confirms the SKILL.md frontmatter, the required sections, and every referenced
file are present, and that no em dash leaked into the skill prose. Exits nonzero
on any problem so CI and the /work loop can gate on it.

Run: python validate_skill.py  (python3 also works).
"""
import re
import sys
from pathlib import Path

SKILL_DIR = Path(__file__).resolve().parent
SKILL_NAME = "subagent-driven-development"

REQUIRED_FRONTMATTER = ["name", "description"]
REQUIRED_SECTIONS = [
    "## Purpose",
    "## The model",
    "## Roster",
    "## The fixed order",
    "## Parallel batches",
    "## Never parallelize",
    "## Prompt templates",
    "## Anti-patterns",
]
REQUIRED_FILES = [
    "references/workflow-contract.md",
    "references/implementer-prompt.md",
    "references/spec-reviewer-prompt.md",
    "references/quality-reviewer-prompt.md",
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

    if errors:
        print(f"FAIL: {SKILL_NAME}")
        for e in errors:
            print(f"  - {e}")
        return 1

    print(f"PASS: {SKILL_NAME} skill structure valid")
    return 0


if __name__ == "__main__":
    sys.exit(main())
