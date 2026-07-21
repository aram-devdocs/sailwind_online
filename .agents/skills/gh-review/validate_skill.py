#!/usr/bin/env python
"""Self-check for the gh-review skill.

Confirms the SKILL.md frontmatter and required sections, the referenced
workflow-contract, that both review modes and the single-verdict rule are
documented, and that all three verdict tokens appear. Exits nonzero on any
problem.

Run: python validate_skill.py  (python3 also works).
"""
import re
import sys
from pathlib import Path

SKILL_DIR = Path(__file__).resolve().parent
SKILL_NAME = "gh-review"

REQUIRED_FRONTMATTER = ["name", "description"]
REQUIRED_SECTIONS = [
    "## Purpose",
    "## When to use",
    "## Modes",
    "## Mirror the CI gates",
    "## File-bucket taxonomy",
    "## Blocker-class findings",
    "## Verdict",
    "## Anti-patterns",
]
REQUIRED_FILES = [
    "references/workflow-contract.md",
]
VERDICT_TOKENS = ["APPROVE", "REQUEST-CHANGES", "REJECT"]
BUCKETS = ["contracts", "api", "server", "client", "infra", "docs"]


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

    for tok in VERDICT_TOKENS:
        if tok not in text:
            errors.append(f"verdict token '{tok}' not documented")

    for b in BUCKETS:
        if b not in text:
            errors.append(f"file bucket '{b}' not documented")

    if "make validate" not in text:
        errors.append("does not mirror the canonical gate 'make validate'")

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
