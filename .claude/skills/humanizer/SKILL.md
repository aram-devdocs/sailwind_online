---
name: humanizer
version: 1.0.0
description: |
  Strip AI-tell patterns from prose before it lands in docs or PR text. A
  catalog of the words, punctuation, and sentence shapes that mark machine
  writing, each paired with the fix. Run it over any markdown you wrote.
allowed-tools: [Read, Edit, Grep]
---

# Humanizer

## Purpose

Make repository prose read like a person wrote it. The banned-vocabulary rule
in `.claude/rules/documentation.md` is the gate; this skill is the technique
for passing it and for catching the softer tells the lint cannot see.

## Process

1. Grep the target file for the banned words and the punctuation tells below.
2. Rewrite each hit using the paired fix. Preserve meaning; cut filler.
3. Re-read the result once for the sentence-shape tells, which no grep catches.
4. Leave RFC 2119 keywords (MUST, SHOULD, MAY) intact where they carry a
   normative line with a because clause; those are load-bearing, not tells.

## Banned vocabulary (replace on sight)

- utilize -> use
- leverage -> use
- comprehensive -> full, or name the scope
- robust -> reliable, or state the property
- pivotal -> central, key, or cut
- delve -> look at, dig into
- enhance -> improve, add to
- foster -> build, support
- showcase -> show
- tapestry, landscape -> name the actual thing
- testament (to) -> shows, proves
- underscore -> stress, or cut

## Punctuation and formatting tells

- Em dashes (`--` or the character): rewrite as a comma, a period, or
  parentheses. This repo bans them outright.
- Title-Case Headings On Every Word: use sentence case.
- Bulleted lists where a sentence would do: prose is fine; not everything is a
  list.
- Bold scattered mid-sentence for emphasis: cut it; let the words carry weight.

## Sentence-shape tells (read, do not grep)

- "It's not just X, it's Y" and "X isn't merely Y" constructions: state the
  claim once, directly.
- "In today's fast-paced world" and other throat-clearing openers: delete the
  opener, start at the point.
- Tricolon padding ("fast, reliable, and scalable") where one adjective is
  true and two are filler: keep the true one.
- Hedging stacks ("it may perhaps sometimes"): pick one modality.
- Summary sentences that restate the paragraph they close: cut them.

## Output

The edited file, plus a short note listing which tells were found and fixed, so
the next reader can spot the pattern faster than the grep did.
