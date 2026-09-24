# 0007: JSON-RPC over `wispd attach`, locally and over SSH

- Status: accepted
- Date: 2026-09-24
- Issue: #56

## Context

The editor and `wispd` need one protocol, whether `wispd` runs on this Mac or on a host reached over SSH. It has to carry everything from M1's projects to M6's triggers. Agents on a host keep running while the MacBook sleeps (M4), so the editor has to catch up on what it missed. #57 to #66 build on this record. The prototypes that checked it are described on #56.

## Decision

### Transport

- **Local:** wispd listens only on a Unix socket, `wispd.sock` in its data folder `~/Library/Application Support/wisp` (0006).
  - On every start, wispd checks the folder with `symlink_metadata`. It must be a directory, not a symlink, and owned by wispd's euid. wispd then sets it to 0700.
  - A `flock` on `wispd.lock` allows one wispd per folder. Only the lock holder removes an old `wispd.sock`, and only if it is a socket.
  - The socket is 0600, and `getpeereid` must return wispd's own uid.
  - macOS caps socket paths at 103 bytes, which the default path exceeds when the home folder path is longer than 59 bytes.
    - In that case, wispd and its clients (`attach`, `wispd mcp`) all use `$(getconf DARWIN_USER_TEMP_DIR)wispd-<hash>.sock`. `<hash>` is the first 8 hex digits of the SHA-256 of the data folder's path, and that folder is per user and 0700.
    - macOS's daily `dirhelper` deletes files there that are older than three days, so wispd checks the socket every minute and binds it again if it is gone.
  - There is no TCP port, no token, and no system-wide daemon. Each macOS user runs their own wispd, so users of a shared Mac stay isolated.
  - The trust boundary is the user. Any process running as that user can connect and call any method, including the agents wispd starts, which run shell commands. #11's plan approval is a UX step, not a boundary against a compromised agent.
- **Editor:** it always talks over a child process's stdio.
  - Locally it runs the bundled `wispd attach` (#62). For a host it runs `ssh -T -o BatchMode=yes -o ConnectTimeout=10 -o ControlPath=none -- <destination> wispd attach`.
  - The editor rejects a destination that starts with `-` or contains whitespace or control characters, and `--` keeps ssh from reading it as an option. `wisp.host` is application-scoped, so a workspace's `.vscode/settings.json` can't set it.
  - `ControlPath=none` keeps a reconnect after sleep from reusing a stale shared connection. The cost is that hosts needing interactive 2FA are out of scope, because `BatchMode` could only reach them through a shared connection.
  - `attach` (#60) bridges stdio to the socket byte for byte. If wispd isn't running, `attach` starts it through the LaunchAgent when #61 installed one.
    - Otherwise it starts `serve` in a new session (`setsid`), with stdio going to its log, so the ssh session can close.
    - #60's "no orphaned processes" applies to `attach`, not to that `serve`, so a disconnect never stops agents.
    - On a host, the LaunchAgent is the recommended setup, since a process started from SSH may not reach the Keychain (0004).
- **Credentials:** wisp stores none and never sees any.
  - The user's `ssh` applies their config, keys, agent, `known_hosts`, and jump hosts.
  - `BatchMode=yes` turns prompts into errors. The user accepts a new host key or unlocks a key once, with `ssh <destination>` in the integrated terminal.
- **Reconnect:**
  - The editor sends `host/health` every 30 s and on wake. If 10 s then pass with no bytes received, it kills the child and reconnects, backing off from 1 s to 10 s (#11). This covers both sleep and network changes.
  - Any received bytes, such as a replay in progress, reset that timer.
  - wispd writes responses ahead of queued events, and drops a connection that has been silent for 90 s.

### Framing

- **Messages:** JSON-RPC 2.0 in both directions, one compact JSON object per line (NDJSON). MCP's stdio transport and the vendor CLIs (0004) use the same framing.
- **Frame size:** at most 8 MiB (`maxFrameBytes`).
  - A malformed line gets -32700.
  - An oversized frame closes the connection.
  - Diffs, logs, and files come through paged methods.
- **Conventions:** fields are camelCase, ids are UUIDv7 strings, times are RFC 3339 UTC, and integers stay below 2^53.
- **Rust:** `serde_json` and `tokio_util`'s `LinesCodec::new_with_max_length`, with no RPC framework.
- **TypeScript:** Code - OSS's own `JsonRpcProtocol` (`vs/base/common/jsonRpcProtocol.ts`, which upstream's MCP client uses) over `StreamSplitter('\n')`, so no npm dependency is added.
- **Debugging:** `printf '%s\n' '<request>' | ssh <destination> wispd attach | jq`, which works because `attach` half-closes.

### Handshake and versioning

- **`initialize` comes first**, and anything sent before it fails with `notInitialized`.
  - It sends `protocol: {min, max}`, `client: {name, version, machineId?}`, and `capabilities`.
  - It returns `protocol`, `wispd` (the release version), `logId`, `capabilities`, and `maxFrameBytes`.
  - `capabilities` is an object map such as `{"agents": {}}`, as in LSP and MCP.
  - The map, the version fields, and the `incompatibleProtocol` error never change shape.
- **`protocol` is an integer, starting at 1.**
  - Additions keep it: methods, notifications, event kinds, optional fields, and enum values. Receivers ignore anything unknown, and every enum Rust receives has an `#[serde(other)]` fallback.
  - An older wispd would silently ignore a new option. So an option whose loss changes behavior, such as a sandbox setting, is sent only when wispd advertises a capability for it.
  - Removals, renames, and type changes bump it. After a bump, the previous version stays in range for at least one release, so an editor and its host can upgrade at different times.
  - Capabilities (`agents`, `coordinator`, `localRunner`, `triggers`) gate later features, so a newer editor still works with an older host.
- **On a mismatch**, wispd answers `incompatibleProtocol`, with both ranges and its release version.
  - The editor enters #63's incompatible state and stops retrying until the user clicks Retry.
  - It names the side to update, for example "Update wisp on mac-mini".

### Events and backpressure

- **Event log:** every state change goes into one event log, numbered by a daemon-wide `seq`.
  - From M3 the log is stored in SQLite (#58).
  - `logId` changes only when the log starts over, such as after a wiped data folder or after an M1 restart that loses an in-memory log.
- **Subscribing:** snapshot methods such as `project/list` return the `seq` they reflect. `events/subscribe {after, project?}` replays newer events, then streams live `events/event` notifications: `{subscription, seq, time, project?, event: {kind, ...}}`. Without `project`, it gets host-level events such as `project.created`.
- **Several clients:** any number of connections may attach, each with its own subscriptions, and wispd broadcasts every change to all of them.
- **Resuming:** after a reconnect, the editor resubscribes from its last `seq`. It reloads its snapshots instead if `logId` changed, or if wispd answers `resyncRequired` because the history is gone or too long to replay. From M3, `agent/output` rebuilds a running agent's transcript after a resync.
- **Backpressure:** each connection has a bounded outbound queue.
  - A subscriber that falls behind the live buffer reads from the log until it catches up, so agents never wait on an editor and memory stays bounded.
  - Output is coalesced to one `agent.output` per run every 50 ms.

### Errors and cancellation

- **Error codes:**
  - Malformed traffic gets JSON-RPC's standard codes.
  - -32800 means cancelled.
  - Every wisp error is -32000 with `data: {kind, detail?}`. The editor matches on `kind`, a generated enum such as `projectNotFound`, and never on `message`.
- **Cancelling a request:** `$/cancelRequest {id}` cancels it, and the request still gets exactly one answer.
- **Disconnects:** a disconnect fails pending requests but never stops an agent. Long work takes a client-generated id (`runId`) and stops only through its own method (`agent/stop`), which uses 0004's cancel for each CLI.
- **Idempotent creates and starts:** every method that creates or starts something takes the new thing's id from the caller: `project/create {id}`, `agent/start {runId}`, `coordinator/send {turnId}`, `runner/start {runId}`, and `trigger/create {id}`.
  - The caller generates the id (UUIDv7) once and sends the same id on every retry.
  - If a thing with that id exists, the receiver returns it instead of creating or starting another. If the params differ from the original call, it fails with `idConflict`.
  - A retry after a lost connection therefore never starts a second agent or spends quota twice.

### Types

- **Source of truth:** the `wisp-protocol` crate (#57), which holds the serde types and one method table.
- **Generated TypeScript:** an explicit generator command runs ts-rs 12 with `Config::with_large_int("number")`.
  - It doesn't use `#[ts(export)]`, whose generated tests write files during `cargo test`.
  - The output is committed under `editor/`, which the fork job's input hash covers, so a type change also reruns the fork type-check. The fork still builds without Rust.
- **Tests (#57):** `check-rust` already runs `cargo test`, which runs both of these.
  - A staleness test regenerates the TypeScript in memory and fails if it differs from the committed copy. Codex's app-server crate uses the same check.
  - Sample messages for each protocol version are committed, and a test asserts that they all still deserialize. This enforces the additive rule.

### Methods

| M1 method | Direction | Purpose |
| --- | --- | --- |
| `initialize` | editor to wispd | Handshake |
| `host/health` | editor to wispd | Uptime, store state, running agents; heartbeat |
| `host/version` | editor to wispd | wispd and protocol versions, macOS, arch |
| `project/list` | editor to wispd | `{projects, seq}` |
| `project/create` | editor to wispd | `{id, name, repoPath}`; idempotent on `id` |
| `events/subscribe`, `events/unsubscribe` | editor to wispd | Replay after `seq`, then live |
| `events/event` | wispd to editor | One event, such as `project.created` |
| `$/cancelRequest` | either | Cancels a request |

Later milestones add methods and events behind a capability, with no version bump:

| Capability | Methods | Events |
| --- | --- | --- |
| M3 `agents` | `agent/start {runId, ...}`, `agent/stop`, `agent/list`, `agent/output {runId, after}` (paged, returns `seq`), `agent/diff` (paged), `context/list`, `context/read`, `context/write` | `agent.started`, `agent.output`, `agent.finished`, `agent.diffReady`, `context.changed` |
| M4 `coordinator` | `coordinator/send {turnId, ...}`, `coordinator/stop`, `plan/approve {planId}`, which fails with `planReplaced` if #11's flow replaced that plan | `coordinator.output`, `coordinator.turnFinished`, `plan.proposed`; parallel agents are just more `runId`s |
| M5 `localRunner` | `runner/start {runId, ...}`, `runner/stop`, and the shared context mirror sync (0005), which the host sends over the connection the MacBook opened. `client.machineId` tells two machines apart. | `runner.output`, `runner.finished` |
| M6 `triggers` | `trigger/list`, `trigger/create {id, ...}` | `trigger.fired` |

`wispd mcp` (M4) is an MCP server on stdio and a normal wisp client on the socket. It sends `initialize` and heartbeats like any other client, and exposes only its project's coordinator tools (0004), never `plan/approve`.

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
- **Additive changes:** the protocol reaches M6 without a version bump only while changes stay additive. The sample-message tests and reviews of `wisp-protocol` enforce that.
- **Upgrades:** after an upgrade, the old wispd keeps running until it restarts (#71).
