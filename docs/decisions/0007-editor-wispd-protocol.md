# 0007: JSON-RPC over `wispd attach`, locally and over SSH

- Status: accepted
- Date: 2026-09-24
- Issue: #56

## Context

The editor and `wispd` need one protocol, whether `wispd` runs on this Mac or on a host reached over SSH. It has to carry everything from M1's projects to M6's triggers. Agents on a host keep running while the MacBook sleeps (M4), so the editor has to catch up on what it missed. #57 to #66 build on this record. The prototypes that checked it are described on #56.

## Decision

### Transport

- **Local:** wispd listens only on the Unix socket `~/Library/Application Support/wisp/wispd.sock` (0006).
  - The folder is 0700 and the socket 0600, and `getpeereid` must return wispd's own uid.
  - A `flock` on `wispd.lock` allows one wispd per folder, and the lock holder removes a stale socket before binding.
  - There is no TCP port, no token, and no system-wide daemon. Each macOS user runs their own wispd, so users of a shared Mac stay isolated.
- **Editor:** it always talks over a child process's stdio.
  - Locally it runs the bundled `wispd attach` (#62). For a host it runs `ssh -T -o BatchMode=yes -o ConnectTimeout=10 -o ControlPath=none -- <destination> wispd attach`.
  - The editor rejects a destination that starts with `-` or contains whitespace or control characters, and `--` keeps ssh from reading it as an option. `wisp.host` is application-scoped, so a workspace's `.vscode/settings.json` can't set it.
  - `ControlPath=none` keeps a reconnect after sleep from reusing a stale shared connection. The cost is that hosts needing interactive 2FA are out of scope, because `BatchMode` could only reach them through a shared connection.
  - `attach` (#60) bridges stdio to the socket byte for byte. It starts wispd if needed, through the LaunchAgent when #61 installed one.
- **Credentials:** wisp stores none and never sees any.
  - The user's `ssh` applies their config, keys, agent, `known_hosts`, and jump hosts.
  - `BatchMode=yes` turns prompts into errors. The user accepts a new host key or unlocks a key once, with `ssh <destination>` in the integrated terminal.
- **Reconnect:**
  - The editor sends `host/health` every 30 s and on wake. After 10 s with no answer, it kills the child and reconnects, backing off from 1 s to 10 s (#11). This covers both sleep and network changes.
  - wispd drops a connection that has been silent for 90 s.

### Framing

- **Messages:** JSON-RPC 2.0 in both directions, one compact JSON object per line (NDJSON). MCP's stdio transport and the vendor CLIs (0004) use the same framing.
- **Frame size:** at most 8 MiB (`maxFrameBytes`).
  - A malformed line gets -32700.
  - An oversized frame closes the connection.
  - Diffs, logs, and files come through paged methods.
- **Conventions:** fields are camelCase, ids are UUIDv7 strings, times are RFC 3339 UTC, and integers stay below 2^53.
- **Rust:** `serde_json` and `tokio_util`'s `LinesCodec::new_with_max_length`, with no RPC framework.
- **TypeScript:** Code - OSS's own `JsonRpcProtocol` (`vs/base/common/jsonRpcProtocol.ts`, which upstream's MCP client uses) over `StreamSplitter('\n')`.
  - That needs no new npm dependency.
  - `vscode-jsonrpc` is only a transitive dependency in 1.139.0, through `@github/copilot-sdk` and the dev-tunnels packages.
- **Debugging:** `printf '%s\n' '<request>' | ssh <destination> wispd attach | jq`, which works because `attach` half-closes.

### Handshake and versioning

- **`initialize` comes first**, and anything sent before it fails with `notInitialized`.
  - It sends `protocol: {min, max}`, `client`, and `capabilities`.
  - It returns `protocol`, `wispd` (the release version), `logId`, `capabilities`, and `maxFrameBytes`.
  - Its version fields and the `incompatibleProtocol` error never change shape.
- **`protocol` is an integer, starting at 1.**
  - Additions keep it: methods, notifications, event kinds, optional fields, and enum values. Receivers ignore anything unknown, and every enum Rust receives has an `#[serde(other)]` fallback.
  - Removals, renames, and type changes bump it.
  - Capabilities (`agents`, `coordinator`, `localRunner`, `triggers`) gate later features, so a newer editor still works with an older host.
- **On a mismatch**, wispd answers `incompatibleProtocol`, with both ranges and its release version.
  - The editor enters #63's incompatible state and stops retrying until the user clicks Retry.
  - It names the side to update, for example "Update wisp on mac-mini".

### Events and backpressure

- **Event log:** every state change goes into one event log, numbered by a daemon-wide `seq`.
  - From M3 the log is stored in SQLite (#58).
  - `logId` changes only when the log starts over, such as after a wiped data folder or after an M1 restart that loses an in-memory log.
- **Subscribing:** snapshot methods such as `project/list` return the `seq` they reflect. `events/subscribe {after, project?}` replays newer events, then streams live `events/event` notifications: `{subscription, seq, time, project?, event: {kind, ...}}`. Without `project`, it gets host-level events such as `project.created`.
- **Resuming:** after a reconnect, the editor resubscribes from its last `seq`. It reloads its snapshots instead if `logId` changed, or if wispd answers `resyncRequired` because the history is gone or too long to replay.
- **Backpressure:** each connection has a bounded outbound queue.
  - A subscriber that falls behind the live buffer reads from the log until it catches up, so agents never wait on an editor and memory stays bounded.
  - Output is coalesced to one `agent.output` per run every 50 ms.

### Errors and cancellation

- **Error codes:**
  - Malformed traffic gets JSON-RPC's standard codes.
  - -32800 means cancelled.
  - Every wisp error is -32000 with `data: {kind, detail?}`. The editor matches on `kind`, a generated enum such as `projectNotFound`, and never on `message`.
- **Cancelling a request:** `$/cancelRequest {id}` cancels it, and the request still gets exactly one answer.
- **Disconnects:** a disconnect fails pending requests but never stops an agent.
  - Long work returns an id (`runId`) and stops only through its own method (`agent/stop`), which uses 0004's cancel for each CLI.
  - Create methods take a client-generated id, so retrying after a lost connection can't create a duplicate.

### Types

- **Source of truth:** the `wisp-protocol` crate (#57), which holds the serde types and one method table.
- **Generated TypeScript:** ts-rs 12 generates it with `Config::with_large_int("number")`. It is committed in the editor's wisp-owned source, so the fork builds without Rust.
- **Staleness check:** a `wisp-protocol` test regenerates the TypeScript in memory and fails if it differs from the committed copy. `check-rust` already runs `cargo test`, and Codex's app-server crate uses the same check.

### Methods

| M1 method | Direction | Purpose |
| --- | --- | --- |
| `initialize` | editor to wispd | Handshake |
| `host/health` | editor to wispd | Uptime, store state, running agents; heartbeat |
| `host/version` | editor to wispd | wispd and protocol versions, macOS, arch |
| `project/list` | editor to wispd | `{projects, seq}` |
| `project/create` | editor to wispd | `{id, name, repoPath}`, safe to retry |
| `events/subscribe`, `events/unsubscribe` | editor to wispd | Replay after `seq`, then live |
| `events/event` | wispd to editor | One event, such as `project.created` |
| `$/cancelRequest` | either | Cancels a request |

Later milestones add methods and events behind a capability, with no version bump:

| Capability | Methods | Events |
| --- | --- | --- |
| M3 `agents` | `agent/start` (returns `runId`), `agent/stop`, `agent/list`, `agent/diff` (paged), `context/read`, `context/write` | `agent.started`, `agent.output`, `agent.finished`, `agent.diffReady`, `context.changed` |
| M4 `coordinator` | `coordinator/send`, `coordinator/stop`, `plan/approve`; the coordinator's MCP tools (0004) connect through `wispd mcp`, a second stdio bridge | `coordinator.output`, `coordinator.turnFinished`, `plan.proposed`; parallel agents are just more `runId`s |
| M5 `localRunner` | `runner/start`, `runner/stop`, and the shared context mirror sync (0005), which the host sends over the connection the MacBook opened | `runner.output`, `runner.finished` |
| M6 `triggers` | `trigger/list`, `trigger/create` | `trigger.fired` |

## Alternatives

| Alternative | Why it lost |
| --- | --- |
| gRPC (tonic, `@grpc/grpc-js`) | It needs HTTP/2 end to end, which over `ssh` means a proxy or port forwarding. It adds grpc-js and protoc to the fork (0002 rule 3), and binary frames can't be read with `jq`. |
| HTTP plus WebSocket (axum, `ws`) | Upstream's agent host uses it with a bearer token. On a private link it only adds a layer. On TCP, any local user or browser tab can reach it, so it needs a token, which is a secret. |
| Content-Length framing (LSP) | Hard to type or read, and a bad header loses the stream, while NDJSON resyncs at the next newline. |
| SSH port or socket forwarding | Another process to supervise, it can't start wispd, and servers often disable it. A forwarded TCP port exposes wispd to every local user. |
| In-process SSH (`ssh2`) | Upstream's remote agent host (a 2,180-line file) reparses `ssh_config` and `known_hosts` and prompts for passphrases, so wisp would handle secrets. |
| Schema first, specta, typeshare | Schema first needs two generators. specta 2 is still a release candidate. typeshare rejects `{kind, ...}` enums. |

## Consequences

- **One client:** #63 and #64 share one client that spawns a process and exchanges lines, and #63 gets auto-start from `attach`.
- **Additive changes:** the protocol reaches M6 without a version bump only while changes stay additive. Reviews of `wisp-protocol` enforce that.
- **Remote `PATH`:** a non-interactive SSH `PATH` lacks `/opt/homebrew/bin` by default, so #64 retries with the full path when the remote shell exits 127.
- **Noisy shells:** until `initialize` answers, the editor skips and logs any non-JSON lines printed by the host's shell startup files.
- **Upgrades:** after an upgrade, the old wispd keeps running until it restarts (#71).
