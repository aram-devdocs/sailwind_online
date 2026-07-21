#!/usr/bin/env python
"""Self-check for the session-continuity skill.

Confirms the SKILL.md frontmatter and required sections, the referenced
workflow-contract, that the five whiteboard sections and the 200-line cap are
documented, and that the skill references the two hooks it must not duplicate.
Exits nonzero on any problem.

Run: python validate_skill.py  (python3 also works).
"""
import re
import sys
from pathlib import Path

SKILL_DIR = Path(__file__).resolve().parent
SKILL_NAME = "session-continuity"

REQUIRED_FRONTMATTER = ["name", "description"]
REQUIRED_SECTIONS = [
    "## Purpose",
    "## The whiteboard",
    "## What the hooks do",
    "## Save protocol",
    "## Restore protocol",
    "## Run state contract",
    "## Anti-patterns",
]
REQUIRED_FILES = [
    "references/workflow-contract.md",
]
WHITEBOARD_SECTIONS = [
    "Current state",
    "In progress",
    "Next",
    "Decisions",
    "Blockers",
]
REFERENCED_HOOKS = [
    ".claude/hooks/save-session.sh",
    ".claude/hooks/context-loader.sh",
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

    for w in WHITEBOARD_SECTIONS:
        if w not in text:
            errors.append(f"whiteboard section '{w}' not documented")

    if "200" not in text:
        errors.append("200-line cap not documented")

    # The skill documents the hooks; it must reference them by path, not
    # reimplement them, because the hooks are the source of truth for the wiring.
    for hook in REFERENCED_HOOKS:
        if hook not in text:
            errors.append(f"hook '{hook}' not referenced")
        repo_root = SKILL_DIR.parents[2]
        if not (repo_root / hook).is_file():
            errors.append(f"referenced hook '{hook}' does not exist on disk")

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
