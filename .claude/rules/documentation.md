---
globs: ["docs/**", "*.md"]
---

# Documentation

- Use RFC 2119 keywords (MUST, SHOULD, MAY) for normative statements only, and
  give each one a short because clause, because a rule without a reason invites
  a workaround and narrative prose without keywords reads as optional.
- The banned vocabulary MUST NOT appear: utilize, leverage, comprehensive,
  robust, pivotal, delve, enhance, foster, showcase, tapestry, testament,
  underscore, landscape. Em dashes MUST NOT appear either, because these are
  the AI-tell patterns the humanizer skill exists to strip.
- Version numbers, absolute paths, and churning tool versions live only in
  `docs/04-reference/`, because that is the substitutable layer and the rest
  of `docs/` stays timeless.
- No PM artifacts belong in the repo (status pages, dev logs, plan or backlog
  files), because GitHub issues and PRs are the durable state store. The only
  sanctioned exception is `.claude/lessons-learned.md`.
- The README MUST index every file under `docs/`, because an unindexed doc is
  a doc nobody finds.
