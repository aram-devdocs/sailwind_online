=== issue ===
title: Parent runbook: harden wire-input validation across the server
labels: epic, area:server
milestone: M2
body:
## Context
This parent tracks the child issues that add bounds checks to every field the
Rust server reads from the wire. It is the plan document; each child is a scoped
task under it.

## Child issues
- Validate frame header lengths in sw-net.
- Bound the entity-count field in the world snapshot decode.
- Add a per-connection rate limit to the message loop.

## Acceptance criteria
- [ ] Every child issue below is closed.
- [ ] `make validate` stays green throughout.

=== issue ===
title: Validate frame header lengths in sw-net
labels: P1, area:server
milestone: M2
blocked-by: None
body:
## Context
The frame decoder trusts the declared length before reading it. A hostile client
can declare a length larger than the buffer.

## Acceptance criteria
- [ ] The decoder rejects a frame whose declared length exceeds the read buffer.
- [ ] A failing test covers the oversized-length case first.

=== issue ===
title: Bound the entity-count field in the world snapshot decode
labels: P1, area:server
milestone: M2
blocked-by: None
body:
## Context
The snapshot decoder loops on an attacker-controlled count with no ceiling.

## Acceptance criteria
- [ ] The decode caps the entity count at the documented maximum.
- [ ] A failing test drives an over-cap count into the decoder first.
