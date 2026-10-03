# 0041: Threads have lineage, and every thread gets the host-wide Parallax MCP

- Status: accepted; the store and protocol are PLX-369's, the thread tools and Claude Code's server are PLX-373's, and they replace [0019](0019-coordinator-mcp-tools.md)'s eight tools in PLX-380
- Date: 2026-10-03
- Issue: PLX-368, PLX-369, PLX-373

## Context

Agents in Parallax can't act on other threads. `plxd mcp` (0019) gives only a Project's coordinator eight tools, bound to its Project. Normal threads, Codex, and Cursor get none. The only link between runs is `runs.coordinator_thread`, which only a coordinator's runs carry. A thread's title lives in the app's `localStorage`, so plxd, agents, and other devices can't read or set it. Claude's own Task subagents show as a flat "Running agent" row. Ryan asked for the rework on 2026-10-03 (PLX-368) and picked child chips in the top bar for the lineage UI.

## Decision

### Lineage in the store (PLX-369)

- **Parent.** A run's parent is the run that launched it, in `runs.parent`. It lives on `runs`, not `threads`, because a coordinator's subagents are runs with no thread row. `agent/start` with `coordinatorThread` records that thread as the parent. A coordinator, whose `coordinatorThread` is its own id (0024), has none. Migration 23 backfills existing runs the same way. `parent` is part of the start methods' idempotent params.
- **Fork origin, title, settled.** A thread gets `forked_from_run` and `forked_from_turn`, `title`, and `settled` (default false) on `threads`. Nothing sets the fork origin until fork (PLX-375).
- **Wire.** `Thread` gains `parent?` (a run id), `forkedFrom? {run, turn}`, `title?`, and `settled?`. `thread/start` takes `parent?` and `title?`. `parent` must name an existing run, or the start fails with `runNotFound`. `title` isn't part of the retry check, since it can change.
- **`thread/update`** also takes `title?` and `settled?`. A title is trimmed, empty clears it, and over 256 bytes fails with `invalidParams`. Each change appends `thread.updated`, as 0033's fields do.
- **Deleting.** Deleting a run, through `thread/delete` or `project/delete`, clears it from its children's `parent` and its forks' `forkedFrom` in the same transaction, and each such thread gets `thread.updated`. A child never points at a run that is gone, and a delete never fails because of children.
- **Capability.** All of this is behind `threadLineage`, since an older plxd would silently drop the new params.

### The host-wide MCP (PLX-373)

- plxd starts `plxd mcp --thread <runId> [--data-dir <dir>]` for every thread. plxd sets the caller's run id, never the model, as 0019 binds a coordinator, so a launch records the caller as the child's parent. At startup the server checks the run exists, and exits 1 if it doesn't. Framing, a connection per call, `isError` results, and caps are 0019's.
- **Claude Code (PLX-373).** A thread that runs as full Claude Code (0034: `approvals`, or Bypass Permissions) gets `--mcp-config` with the server, with no `--strict-mcp-config`, so it joins the user's, the repository's, and plugins' servers, and `--allowedTools` names the tools before the todo tools, so they run in every mode. A thread without `approvals` keeps 0013's sandbox and gets no tools, since the server runs outside the sandbox. Routing drops the tools from every run that isn't a thread. A coordinator keeps 0019's eight tools until PLX-380 replaces them. Codex and Cursor threads get them in PLX-379.
- The tools. Every argument struct rejects unknown fields, and none takes the caller's id. `runId` names a target run anywhere on the host:

| Tool | Arguments | plxd methods |
| --- | --- | --- |
| `thread_list` | `includeArchived?` | `thread/list` and `agent/list`; newest first, each with run id, title, status, backend, model, mode, repo, branch, parent, settled, archived, and `you` for the caller |
| `thread_read` | `runId`, `after?` (an event `seq`) | `agent/events`, rendered as a transcript: messages with their sender (User, Parallax, or `Thread <id>`), replies, tool calls and results at 300 bytes each, interrupts, and how each run ended. A page is about 64 KiB and ends with the `after` for the next one |
| `thread_search` | `query`, `limit?` (at most 100) | `thread/search` (0047) |
| `thread_launch` | `prompt`, `threads?`, `title?`, `backend?` or `account?` (a key account id), `model?`, `effort?`, `mode?`, `workspace?` (`worktree`, `checkout`, `none`), `repo?` (an id or absolute path), `base?` (worktree), `branch?` (checkout) | `thread/start` with `parent` set to the caller and `approvals`. Defaults: the caller's repo in a new worktree, or no repo if it has none; the caller's mode when the child runs on the caller's backend. A path with no repo entry is registered with `repo/add` |
| `thread_send` | `runId`, `text`, `threads?` | `agent/send` with `from` set to the caller; queued behind a running turn |
| `thread_wait` | `runId`, `timeoutSeconds?` (default 300, at most 1800) | `agent/list` every 0.5 s, each on a new connection, until the run is neither `starting` nor `running`; returns `idle`, `timedOut`, the run, and the last 8 KiB of its last output |
| `thread_interrupt` | `runId` | `agent/cancel` with `from` set to the caller |
| `thread_update` | `runId?` (default the caller), `title?`, `settled?`, `archived?` | `thread/update`, then `thread/archive` |
| `pr_link`, `pr_unlink` | `url`, `runId?` (default the caller) | `pr/link`, `pr/unlink` |

- **A child gets no more permission than its caller.** `thread_launch` refuses a `mode` that needs less approval than the caller's, in the order `plan`, `manual`, `auto`, `edit`, `bypass`, and a mode plxd doesn't know. No mode means `edit`, on either side. The refusal is a tool error naming both modes. A Plan thread can launch only Plan children, and only a Bypass thread can launch a Bypass one, so a prompt injection in a thread that asks before each write can't start a run that doesn't.
- `thread_send`, `thread_wait`, and `thread_interrupt` refuse the caller itself, which can't message, wait on, or stop its own turn from inside it. Prompts and messages are at most 64 KiB, and a result at most 256 KiB, as 0019's.
- **Provenance.** `agent/send` and `agent/cancel` take `from?`, a run that must exist, or the call fails with `runNotFound`. The message's `turnStarted` carries `from`, and an interrupt of a running run logs an `interrupted {from}` item before its `agent.finished`. The actor keeps a message's sender in memory until its `turnStarted`, as waiting messages are kept today. A provider handoff's conversation names such a message's sender as `Thread <id>`. The app shows such a message folded away as "From another thread: <title>", as it shows a wake-up, leaves it out of the composer's Up and the prompt rail, and shows an interrupt as "Stopped by another thread: <title>".
- **`pr/link` and `pr/unlink`** take `{runId, url}`. A link must be a GitHub pull request URL, or it fails with `invalidParams`. Both answer with the run and report it as `agent.updated`.
- **Capability.** The new params, items, and methods are behind `threadTools`. The app allows the two methods from its renderer.
- **Deferred.** `thread_send`'s `delivery: queue | steer` waits for PLX-370's durable queue and steer; until then a message queues. `thread_fork` is PLX-375's.
- They replace 0019's eight tools in PLX-380. A coordinator becomes an ordinary parent thread, and 0025's wake-ups become "a parent wakes when a child it launched finishes".
- **Host-wide writes, with provenance.** Write tools reach any thread on the host, not only the caller's children. The target thread's transcript records which thread sent each message or interrupt, so the user can tell an agent's message from their own.

### Native subagents

Claude's Task subagents show as read-only children of their thread, built from each event's `parent_tool_use_id`. They aren't threads: they have no run, no composer, and no MCP. Only `thread_launch` children are full threads.

## Consequences

- One parent column covers both a coordinator's subagents and launched threads, so the lineage UI (PLX-374) reads one field. `AgentRun` doesn't carry `parent` yet; `coordinatorThread` still marks a coordinator's subagents on the wire.
- Titles move to plxd, so every client and agent sees the same one. The app still shows the prompt's first line when a thread has no title, and still keeps its own generated titles until it moves them to `thread/update`.
- A deleted parent's children become top-level threads. Nothing records that they had a parent.
- Host-wide write tools let any thread message or stop any other. Provenance in the transcript is the check on that, not a permission.
- Until PLX-380, a coordinator keeps 0019's tools. Until PLX-379, Codex and Cursor threads have none.
- A sender is kept only in plxd's memory until its message starts, so a message that waits across a plxd restart loses its `from`, as it loses the message today. PLX-370's durable queue is where to store it.

## Evidence

On 2026-10-03, on macOS 27.0 with Claude Code 2.1.288 and Ryan's own subscription, `plxd serve` with a fresh data folder ran a thread started over `plxd attach` on `haiku` in Bypass Permissions, with no repo, asked to launch a child on `sonnet`, wait on it, and read it. It called `thread_launch`, `thread_wait`, and `thread_read` in turn. `thread/list` then showed the child with the parent's run id as `parent`, and the child's own session reported `claude-sonnet-5-5`. Asked to list its MCP servers by tool name, a second thread named the user's thirteen claude.ai servers and `plxd`.
