# 0003: Wisp, `wisp`, and `wispd`

Status: accepted
Date: 2026-09-23
Issue: #17, #19

## Context

The plan left two naming questions open: whether to confirm the name wisp, and whether the host daemon, `projectd`, is shared with Roster. Ryan answered both: Wisp is the name, and the daemon should use neither Roster nor the name `projectd`.

## Decision

- The product is **Wisp**. The Homebrew cask token, bundle identifiers, and data folders use `wisp`.
- `wisp` is the editor's command-line launcher, as the plan says.
- The host daemon is **`wispd`**. It lives in `daemon/` and is not shared with Roster.

## Consequences

- Where `docs/PLAN.md` says `projectd`, read `wispd`.
- Two binaries ship: the editor app with its `wisp` launcher, and `wispd`. Nothing has to coordinate with another product.
- The overlap with the Gleam web framework of the same name is accepted.
