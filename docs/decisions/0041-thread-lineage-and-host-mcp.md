# 0041: Threads have lineage, and every thread gets the host-wide Parallax MCP

- Status: accepted; the store and protocol are PLX-369's, the MCP tools' schemas are PLX-373's, and they replace [0019](0019-coordinator-mcp-tools.md)'s eight tools when PLX-373 lands; fork, which sets `forkedFrom`, is [0050](0050-fork-a-thread.md)
- Date: 2026-10-03
- Issue: PLX-368, PLX-369

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

- plxd starts `plxd mcp --thread <runId>` for every thread, on every backend. plxd sets the caller's run id, never the model, as 0019 binds a coordinator, so a launch records the caller as the child's parent.
- The tools, with schemas settled in PLX-373:

| Tool | What it does |
| --- | --- |
| `thread_list` | Lists the host's threads, with lineage |
| `thread_read` | Reads a thread's transcript |
| `thread_search` | Searches threads (PLX-372) |
| `thread_launch` | Starts a thread on any provider, model, mode, and workspace, as the caller's child |
| `thread_send` | Sends a message, queued or as a steer into the current turn (PLX-370) |
| `thread_wait` | Waits for a thread's turn to finish |
| `thread_interrupt` | Stops a thread's turn |
| `thread_update` | Renames, settles, or archives a thread |
| `thread_fork` | Forks a thread at a turn (PLX-375) |
| `pr_link` | Links a pull request to a thread |

- They replace 0019's eight tools. A coordinator becomes an ordinary parent thread, and 0025's wake-ups become "a parent wakes when a child it launched finishes".
- **Host-wide writes, with provenance.** Write tools reach any thread on the host, not only the caller's children. The target thread's transcript records which thread sent each message or interrupt, so the user can tell an agent's message from their own.

### Native subagents

Claude's Task subagents show as read-only children of their thread, built from each event's `parent_tool_use_id`. They aren't threads: they have no run, no composer, and no MCP. Only `thread_launch` children are full threads.

## Consequences

- One parent column covers both a coordinator's subagents and launched threads, so the lineage UI (PLX-374) reads one field. `AgentRun` doesn't carry `parent` yet; `coordinatorThread` still marks a coordinator's subagents on the wire.
- Titles move to plxd, so every client and agent sees the same one. The app still shows the prompt's first line when a thread has no title, and still keeps its own generated titles until it moves them to `thread/update`.
- A deleted parent's children become top-level threads. Nothing records that they had a parent.
- Host-wide write tools let any thread message or stop any other. Provenance in the transcript is the check on that, not a permission.
- Until PLX-373 lands, a coordinator keeps 0019's tools and normal threads have none.
