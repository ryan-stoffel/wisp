# 0003: Wisp, `wisp`, and `wispd`

- Status: accepted
- Date: 2026-09-23
- Issue: #17, #19

## Context

The plan left two naming questions open: whether to confirm the name wisp, and whether the host daemon, `projectd`, is shared with Roster. Ryan answered both: Wisp is the name, and the daemon should use neither Roster nor the name `projectd`.

## Decision

- The product is **Wisp**. The Homebrew cask token, bundle identifiers, and data folders use `wisp`.
- `wisp` is the editor's command-line launcher. The plan says "The command is `wisp`", and the editor is the user-facing program.
- The host daemon is **`wispd`**. Ryan ruled out `projectd` without naming a replacement, so `wispd` is a default chosen in #17, and Ryan can override it. It lives in `daemon/` and is not shared with Roster (Ryan, #17).
- Ryan confirmed the name (#19) without waiting for conflict checks. The GitHub, domain, and trademark checks listed in `docs/PLAN.md` no longer gate the release.

## Consequences

- Where `docs/PLAN.md` says `projectd`, read `wispd`.
- Two binaries ship: the editor app with its `wisp` launcher, and `wispd`. Nothing has to coordinate with another product.
- The overlap with the Gleam web framework of the same name is accepted.
