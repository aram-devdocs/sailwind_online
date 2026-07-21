---
name: find-skills
version: 1.0.0
description: |
  List the skills available in this repository. Reads every
  .claude/skills/*/SKILL.md, extracts its name and description, and prints a
  compact index so an agent can pick the right skill before acting.
allowed-tools: [Read, Glob]
---

# Find skills

## Purpose

Answer "what skills exist here and what does each do" without guessing. The
skill directory is the source of truth; this skill reads it and reports.

## Process

1. Glob `.claude/skills/*/SKILL.md` to enumerate every skill file.
2. Read each match. Parse the YAML frontmatter for `name` and `description`.
3. Ignore any directory without a `SKILL.md`, because a skill without a
   manifest is not invokable.
4. Sort by `name` for a stable listing.

## Output

A markdown list, one row per skill:

    - `/<name>`: <first sentence of description>

Close with the count of skills found. When no `SKILL.md` files exist, say so
plainly rather than inventing entries.
