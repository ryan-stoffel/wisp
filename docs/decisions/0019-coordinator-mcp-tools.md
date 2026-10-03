# 0019: The coordinator's Parallax tools are `plxd mcp`, bound to one project and thread

- Status: accepted; the coordinator that runs with these tools, and the tools' skipping its own run, are in [0024](0024-coordinator-chat.md); the Claude coordinator's flags are superseded by [0027](0027-claude-permission-modes.md); every thread's host-wide tools, `plxd mcp --thread`, are in [0041](0041-thread-lineage-and-host-mcp.md), which replaces these eight tools in PLX-380
- Date: 2026-09-26
- Issue: #195

## Context

The coordinator is a no-write Claude Code or Codex run (0004, 0012). To plan and run subagents it needs tools that act through plxd: start a subagent, check on it, message it, read its diff, and read and write shared context. Both CLIs take extra tools over MCP, and 0007 had already sketched `plxd mcp` as "an MCP server on stdio and a normal Parallax client on the socket" that exposes only its project's tools. #196 runs the coordinator's turns; this record settles what those turns are given.

## Decision

### `plxd mcp`

- **A hidden subcommand**, `plxd mcp --project <id> --coordinator-thread <id> [--data-dir <dir>]`. The coordinator's CLI launches it as a stdio MCP server. plxd writes these arguments into the CLI's MCP config (`backend::CoordinatorTools`). The model never sees or sets them.
- **Framing.** MCP's stdio transport is JSON-RPC 2.0 as newline-delimited JSON, the same as 0007's, so both sides use `parallax_protocol`'s codec and envelope and no MCP library is added. The server answers `initialize`, `ping`, `tools/list`, and `tools/call`. It advertises only `tools` and echoes the client's protocol version when it knows it (2024-11-05 through 2025-11-25), or the newest.
- **A connection per tool call.** Each call connects to plxd's socket, sends `initialize`, and makes one or two requests. So the server needs no heartbeat (0007's 90 s idle limit never applies), and it keeps working after plxd restarts between calls. It never starts plxd: the coordinator it serves is plxd's own child. At startup it checks that its project exists, and exits 1 if it doesn't, or 4 if it can't find the socket.
- **Errors.** A tool that fails, including on plxd's own error, returns an `isError` result carrying plxd's message, so the model sees it and can react. An unknown tool is a JSON-RPC `invalidParams` error.

### The tools

| Tool | Arguments | plxd methods |
| --- | --- | --- |
| `spawn_agent` | `prompt`, `account?` | `agent/start` with the bound project and `coordinatorThread`; a fresh `runId` per call |
| `list_agents` | none | `agent/list {project}` |
| `agent_status` | `runId` | `agent/list {project}` for status and diff stats; `agent/events` for the last text, turn result, or failure message |
| `message_agent` | `runId`, `text` | binding check, then `agent/send` with a fresh `turnId` |
| `cancel_agent` | `runId` | binding check, then `agent/cancel` |
| `agent_diff` | `runId` | binding check, then `agent/diff`, rendered as unified diffs |
| `read_context` | `path?` | `context/list` without a path, `context/read` with one |
| `write_context` | `path`, `content` | `context/write`, writer `coordinator` |

- **Binding.** No tool takes a project or a thread, and every argument struct rejects unknown fields. A tool that takes a `runId` first checks that the run is in `agent/list {project: <bound>}`. Another project's run gets the same answer as a run that doesn't exist. A normal thread's runs are scoped to a repo entry's id (0017), so they fail this check too. `plan/approve` is never exposed (0007).
- **Tagging.** `agent/start` and `AgentRun` gain an optional `coordinatorThread` (`CoordinatorThreadId`, a UUIDv7), stored in `runs.coordinator_thread` (migration 11). It is part of `agent/start`'s idempotent params. The editor (#198) can tell the coordinator's runs from the user's, and #196's wake-ups can find the runs a thread started. Run summaries show the model `startedByThisCoordinator`.
- **Size limits.** A line from the CLI is at most 4 MiB. A longer one gets an error with a null id and ends the server, because the codec can't resynchronize after it. Prompts and messages are at most 64 KiB, well under `agent/*`'s 1 MiB. Context content is at most 1 MiB, plxd's per-file cap (0005), and paths at most 255 bytes. A result carries at most 256 KiB of text and is cut with a note: a large `agent_diff` or `list_agents` would otherwise fill the model's context. A run's prompt shows at most 500 bytes in a summary, and `agent_status` shows the last 8 KiB of output.

### The coordinator's allowlist (Claude Code)

> Superseded in part by [0027](0027-claude-permission-modes.md): the coordinator is full Claude Code in its permission mode, with none of 0004's no-write flags, and its `system/init` tools aren't checked. `--mcp-config` and `--allowedTools` with the eight tools stay, so plxd's tools work in every mode. Since RYA-249 the allowlist also names Claude Code's todo tools after them ([0027](0027-claude-permission-modes.md#the-todo-tools)).

A coordinator run is 0004's no-write command plus two flags. Its `--settings` now also deny reads under Claude Code's shared temp folder ([0024](0024-coordinator-chat.md)):

```sh
claude -p ... --tools Read,Glob,Grep --setting-sources user \
  --settings '{"disableAllHooks":true}' --strict-mcp-config --permission-mode dontAsk \
  --mcp-config '{"mcpServers":{"plxd":{"type":"stdio","command":"<plxd>","args":["mcp","--data-dir","<dir>","--project","<id>","--coordinator-thread","<id>"]}}}' \
  --allowedTools mcp__plxd__spawn_agent,mcp__plxd__list_agents,mcp__plxd__agent_status,mcp__plxd__message_agent,mcp__plxd__cancel_agent,mcp__plxd__agent_diff,mcp__plxd__read_context,mcp__plxd__write_context
```

- `--tools` names only built-in tools, so the MCP tools don't widen it. `--strict-mcp-config` keeps every MCP server but plxd's out, including the repository's `.mcp.json`. `dontAsk` denies any tool that isn't allowed, so `--allowedTools` lists the eight tools by name rather than 0004's `mcp__plxd__*`. All of these flags are in Claude Code 2.1.267's `--help`, and all predate 0013's 2.1.248 floor, which no-write runs don't enforce.
- The second check on `system/init` admits exactly `Read`, `Glob`, `Grep`, `EndConversation`, and those eight names, and only when the tools were attached. Any other tool, including another `mcp__plxd__*` name, stops the run with `policyViolation`, as before.
- `routing::start` drops the tools from every role but the coordinator, and Claude refuses them on a workspace-write run. `routing::snapshot` and `routing::check` are unchanged: #196 calls them around each turn.
- Checked by hand: the installed Claude Code 2.1.267's `claude mcp list`, with a throwaway `CLAUDE_CONFIG_DIR`, reports `plxd mcp` as connected to a running plxd.

## Alternatives

| Alternative | Why it lost |
| --- | --- |
| MCP served by `plxd serve` itself, over HTTP or its socket | Claude Code would need a URL or a socket-speaking client, and binding a project would need a per-coordinator token or path. A secret and a listener are the things 0007 rejected. |
| The model passes `project` on each call and plxd checks it | Nothing to check it against: a coordinator's run is plxd's own child, whichever project it names. Binding by arguments the model can't see makes the wrong project unreachable, not merely refused. |
| One long-lived socket connection with heartbeats | More code (a timer, reconnecting after a plxd restart) for no gain: tool calls are seconds apart at most, and a local handshake costs microseconds. |
| An MCP SDK crate | Adds a dependency for four methods whose framing Parallax already has. |
| `--allowedTools "mcp__plxd__*"` (0004's sketch) | A wildcard would admit a tool added to the server later without a review of the allowlist. Listing names keeps the allowlist, the `system/init` check, and `tools/list` equal, and a unit test enforces it. |

## Consequences

- #196 builds a `CoordinatorTools` from plxd's own executable, its data folder, the project, and the coordinator thread, and passes it on the coordinator's `RunRequest`. The fake backend ignores it, so #196's scripted coordinator must drive `plxd mcp` itself.
- Codex's coordinator (#122) needs its own config: `mcp_servers.plxd` with `default_tools_approval_mode = "approve"` (0004). The tools already declare `destructiveHint: false`.
- `agent_status` reads a run's whole event history for its last output. A from-the-end page on `agent/events` is the fix if long runs make that slow.
- A coordinator can message, cancel, or read the diff of any run in its project, including runs the user started. Only another project's are out of reach.
