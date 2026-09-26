# Decision records

A decision that affects more than one issue gets a record here. Records supersede `docs/PLAN.md` where they differ.

| Record | Decision |
| --- | --- |
| [0001](0001-ci-before-product-code.md) | CI is built before product code, against a stand-in Electron app, which #38 retired for the real `Wisp.app` |
| [0002](0002-editor-fork-strategy.md) | The editor is upstream Code - OSS at a pinned tag plus a patch series |
| [0003](0003-naming.md) | Wisp, `wisp`, and `wispd` |
| [0004](0004-subscription-providers.md) | Subscriptions run through each vendor's official CLI; wisp never handles consumer credentials |
| [0005](0005-shared-context-folder.md) | Shared context is a daemon-owned folder outside the repo |
| [0006](0006-release-versioning-and-packaging.md) | Versions come from release tags; releases are arm64-only and ad-hoc signed until #7; bundle id `io.github.ryan-stoffel.wisp` |
| [0007](0007-editor-wispd-protocol.md) | The editor speaks JSON-RPC 2.0 as newline-delimited JSON through `wispd attach`, locally or over the user's `ssh`; types come from the `wisp-protocol` crate |
| [0008](0008-editor-overlay.md) | `editor/product.json` and `editor/overlay/` reach the editor tree as one commit under the patches, never as a patch |
| [0009](0009-wispd-data-folder-and-project-host.md) | wispd's files, overrides, log, and exit codes in the data folder; projects have no host field |
| [0010](0010-wispd-attach.md) | `wispd attach` starts wispd through the LaunchAgent `io.github.ryan-stoffel.wisp.wispd` or as a detached `serve`, and exits 4 when it never reaches wispd |
| [0011](0011-agents-window-baseline.md) | wisp opens into upstream's Agents window, laid out like Cursor's Projects: a built-in `ISessionsProvider` and wisp-owned sidebar in the overlay, subagents as tool-origin chats behind the Agents pill, and a few patches |
| [0012](0012-account-routing.md) | `AccountChoice` names a subscription by its backend or a key account by id; per-role defaults live in a `role_defaults` store table; a backend serves both credential kinds for its provider, so fallback never switches backends |
| [0013](0013-worker-sandbox.md) | Workers run in their vendor's own OS sandbox: they write only their worktree, the shared context folder, and temp; their commands can't read a denylist of credential stores but do have network access; wispd commits. Claude workers use `--restricted` with Claude Code's Bash sandbox |
| [0014](0014-agent-runs.md) | The `agents` capability is `agent/start`, `send`, `cancel`, `list`, and `events`; a run outlives its CLI processes, wispd commits it after each one, and it resumes by session after a restart; the event log is stored in SQLite; agents get no SSH session variables and a filled-in `PATH` |
| [0015](0015-subagent-chats.md) | Each agent run is a `wisp.agent` chat of its project's session; the subagents' chat agent is the default for agent-mode chat, which upstream needs to send anything; the Agents pill opens wisp's own panel through a presenter hook in `chatDropdownPill.ts`, and shows by default |
| [0016](0016-event-log-retention.md) | The stored event log prunes host and project events by count; a run's events stay until its run row does, which nothing removes yet (#207); the in-memory replay window is also bounded by bytes |
| [0018](0018-pr-visuals-on-request.md) | `screenshots.yml` runs only for PRs with the `screenshots` label and captures only the scenes the body's `wisp-media` block names (`after`, `before-after`, `video`), into a section of the PR body instead of a comment; supersedes the every-PR rule |

Numbers are assigned in order. Take the next free number when you start the record, add a row to this table in the same PR, and link the record from its issue.

## Template

```markdown
# NNNN: Title

- Status: accepted | superseded by NNNN
- Date: YYYY-MM-DD
- Issue: #n

## Context

What forces the decision.

## Decision

What we will do.

## Consequences

What becomes easier, what becomes harder, and what follows from it.
```
