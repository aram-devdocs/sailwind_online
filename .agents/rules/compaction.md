---
description: Save state before compaction and re-orient after it, so a compacted session does not act on a half-remembered task.
alwaysApply: true
---

# Compaction

Compaction drops detail. Persist what you need before it happens and reload it
after.

- Before compaction you MUST write current state to the run's `MEMORY.md`: mark
  each task and phase status, and note which files are in progress, because the
  post-compaction session has only what you wrote down.
- After compaction you MUST re-orient before acting: read `AGENTS.md`, then the
  run state in `MEMORY.md`, then the in-progress files, because acting on a
  half-remembered task edits the wrong thing.
- You MUST NOT resume implementation from memory alone, because the summary that
  survives compaction is lossy and the files on disk are the source of truth.

Wired by the save-session hook (PreCompact) which prompts the save, and the
context-loader hook (SessionStart) which reloads the run state.
