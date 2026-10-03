# 0050: Forking a thread at a turn

- Status: accepted; extends [0041](0041-thread-lineage-and-host-mcp.md) (it sets `forkedFrom`) and reuses [0014](0014-agent-runs.md)'s handoff
- Date: 2026-10-03
- Issue: PLX-375 (part of PLX-368)

## Context

Ryan wants to branch a conversation without losing the original (PLX-368). 0041 stores a thread's fork origin, `forkedFrom {run, turn}`, but nothing sets it. Each CLI handles a fork differently, so the CLIs installed on Ryan's Mac were checked first ([Evidence](#evidence)):

- **Claude Code** forks a session with `--resume <id> --fork-session`. A hidden flag, `--resume-session-at <message uuid>`, cuts the copy at an earlier message. It is left out of `--help`, and it takes the uuid of the turn's last entry in the session file, which plxd doesn't record.
- **Codex** app-server has `thread/fork {threadId, lastTurnId?}` with `thread/start`'s overrides. `lastTurnId` is Codex's own turn id, which plxd doesn't record either.
- **Cursor Agent** has no fork. `agent acp`'s `initialize` offers only `loadSession` and `session/list`.

## Decision

### `thread/fork`, behind `threadFork`

`thread/fork {runId, newRunId, turnId?, account?, model?}` returns `{thread, run}`, as `thread/start` does.

- **The turn.** `turnId` is a follow-up's turn id. The prompt's turn has none of its own, so the parent's run id names it. Absent means the parent's latest recorded turn. A turn the parent doesn't have, or the latest one while the parent is starting or running, is `invalidParams`.
- **The fork** is a new thread in the parent's repo entry, with `forkedFrom` set, no `parent`, and no title. Its run has the parent's prompt, approvals, and options. It runs on `account` if given, or else on the account the parent's session is on. A fork onto another backend keeps only the options that backend maps, and the parent's model only if `model` is absent and the backend is the same.
- **No CLI starts.** The run is recorded `completed`, where the parent's turn ended. The fork's first `agent/send` starts its CLI.
- **Idempotent** on `newRunId`. A retry returns the fork. A run id that is anything but a fork of `runId` (at `turnId`, when given) is `idConflict`.

### Workspace

| Parent | Fork |
| --- | --- |
| Worktree | A new worktree cut from the parent's latest commit: its last commit (`diff.commit`), or else its worktree's base |
| Current checkout | The same checkout |
| No repo | A scratch repository of its own, with the parent's latest commit fetched into it, and a worktree cut from that |

The parent's uncommitted work isn't included, since plxd commits it only when the parent's CLI exits.

### Transcript

The fork's log starts with its own `agent.started`, then the parent's `agent.output` events up to the end of the fork turn, copied with the fork's run id. The copy stops at the first `turnStarted` of a later turn, and approval items are left out because their requests belonged to the parent's CLI. `agent/events` and the live stream show the fork's history like any run's. The fork keeps that history after the parent is deleted.

### The first message

- **Native** when the fork has sent nothing yet and its backend can fork (`Capabilities::fork`), and the parent still exists, on the same backend and account, isn't starting or running, and has had no turn since the fork turn. The backend gets `Resume {fork: true}` with the parent's session and its usage totals, since a forked session's totals carry over. Claude Code runs `--resume <session> --fork-session`. A Codex thread sends `thread/fork` in place of `thread/resume`. The CLI reports the new session, which becomes the fork's.
- **Handoff** otherwise: Cursor, another provider, an earlier turn, or a parent that moved on. This is 0014's handoff from the fork's own log, which holds the parent's transcript cut at the fork turn. It is told the conversation without tool calls, cut from the front to about 64 KiB.

A native fork at an earlier turn needs plxd to record each turn's vendor id (a Claude message uuid, a Codex turn id). That is left to a follow-up.

## Consequences

- A fork at the latest turn keeps the CLI's own context, tool calls included. A fork at an earlier turn, or onto another provider, keeps only what was said.
- If the parent gets a new turn between `thread/fork` and the fork's first message, the fork takes a handoff, so it never sees the parent's later turns.
- A fork's copied history takes its own rows in the event log.
- Images from the parent's turns aren't copied, so `agent/image` on the fork can't serve them.

## Evidence

On 2026-10-03, on macOS 27.0 with Ryan's own logins. Each parent learned APPLE in turn 1 and BANANA in turn 2, and each fork was asked to list the words.

| CLI | Native fork | Result |
| --- | --- | --- |
| Claude Code 2.1.288, haiku | `--resume <id> --fork-session`, from the parent's cwd and from another | New session id, "APPLE, BANANA". The parent's session file was unchanged |
| Claude Code 2.1.288, haiku | Hidden `--resume-session-at <turn 1's reply uuid> --fork-session` | New session id, "APPLE" |
| codex-cli 0.160.0, gpt-6-luna | app-server `thread/fork {threadId}` | New thread id, "APPLE, BANANA". The parent still answered "APPLE, BANANA" |
| codex-cli 0.160.0, gpt-6-luna | app-server `thread/fork {threadId, lastTurnId: <turn 1>}` | New thread id, "APPLE" |
| Cursor Agent 2026.10.01-14929f9 | None: `initialize` offers `loadSession` and `session/list` only | |

The same day, `plxd serve` from this change, with a fresh data folder, ran a two-turn thread in a worktree on each of Claude Code (haiku) and Codex (gpt-6-luna), driven over `plxd attach`:

| Thread | Fork at | First message | Answer |
| --- | --- | --- | --- |
| Claude Code | Latest turn | Native: a new session | "APPLE, BANANA" |
| Claude Code | The prompt's turn | Handoff | "APPLE" |
| Codex | Latest turn | Native: a new thread, no handoff notice | "APPLE, BANANA" |
| Codex | The prompt's turn | Handoff, with its notice | "APPLE" |

`daemon/tests/server/threads/fork.rs` covers a native fork, a handoff onto another provider from a Current checkout thread, and a fork at an earlier turn of a thread with no repo.
