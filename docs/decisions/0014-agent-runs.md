# 0014: Agent runs in wispd

- Status: accepted
- Date: 2026-09-25
- Issue: #156

## Context

M3's done-when is one subagent that completes a task in a worktree and writes to shared context. #156 ties together the backends (#113, #116), routing (#119, 0012), the worker sandbox (#137, 0013), worktrees (#154, #166), shared context (#155, 0005), usage (#120), and the protocol (0007, 0011). 0007 sketched the `agents` capability's methods and events. Building them settled names and behavior that the editor (#105, #157) and the environment question (#96) depend on.

## Decision

### Protocol, behind the `agents` capability

| Method | Params | Result |
| --- | --- | --- |
| `agent/start` | `{runId, project, prompt, policy: "workspaceWrite", account?}` | `{run}` |
| `agent/send` | `{runId, turnId, text}` | `{run}` |
| `agent/cancel` | `{runId}` | `{run}` |
| `agent/list` | `{project?}` | `{runs, seq}` |
| `agent/events` | `{runId, after, limit?}` | `{events, more}` |

- `agent/cancel` is 0007's `agent/stop`, named as #156 asks. `agent/events` replaces 0007's `agent/output`: it pages through all of one run's logged events, not only output, which is what an editor needs to rebuild a transcript after `resyncRequired` or a restart.
- `agent/start` is idempotent on `runId` (0007). A retry with the same project, prompt, policy, and account returns the run; different params fail with `idConflict`. `account` absent means the worker role's default (0012).
- `agent/send` is idempotent on `turnId` while wispd runs. It goes to a running CLI as its next turn. Once the run's CLI has ended, it resumes the vendor session in the same worktree with the message as the prompt (0011: Ryan messages a subagent directly).
- Events, all project-scoped and all carrying `runId`: `agent.started {run?}` (wispd always sends `run`; it is optional only because a v1 sample predicted `agent.started {runId}`), `agent.updated {run}` for any change to the snapshot, `agent.output {items}` coalesced per run every 50 ms, `agent.accountFallback`, `agent.finished {outcome}` once per CLI process, and `agent.diffReady {diff}` after each commit.
- Errors: `runNotFound`, `runNotResumable`, `workerUnavailable` (a backend without 0013, a missing or too old CLI with both versions named, a sandbox path holding `*?[]`), and `worktreeFailed` (for example a dirty repository).

### A run's life

- **Statuses:** `starting`, `running`, `completed`, `failed`, `cancelled`, `interrupted`. A run outlives its CLI processes: `completed`, `failed`, `cancelled`, and `interrupted` runs with a session id take `agent/send`.
- **Before anything is created,** wispd resolves the account (`routing::resolve`, `Role::Worker`), refuses a backend whose `Capabilities::worker_sandbox` is false, checks Claude Code's detected version against `WORKER_MIN_VERSION`, and canonicalizes and checks every sandbox path. Detection reads the version from `claude --version` when `claude auth status` doesn't report one.
- **Then** it creates the worktree, records the run and worktree rows (with #166's pinned `git_dir`), and starts the CLI with `WorkerSandbox::for_worktree`, whose git folder comes from the user's own checkout. The first prompt tells the agent its limits (0013).
- **One actor task per run** takes commands and backend events in one loop. It charges usage to the run's current account, which changes on `AccountFallback` (0012).
- **When a CLI process ends,** wispd commits the worktree on the run's branch with `commit_all` (#166: pinned git folder, no hooks), whatever the outcome, and reports the commit's stats against the worktree's base. A failed commit fails the run with `commitFailed`.
- **When wispd stops,** it cancels running CLIs and records their runs `interrupted`, without committing. **When wispd starts,** any run still `starting` or `running` (a crash) becomes `interrupted`. Both resume through `agent/send` by their session id.

### The event log is stored

0007 moved the log to SQLite in M3. The `events` table holds every event with its daemon-wide `seq`, its project, and its run. `log_meta` holds `logId`. So `logId` and `seq` now survive a restart, and an editor resubscribes from its last `seq` instead of reloading. `events/subscribe` still replays only from the newest `retention` events in memory (10,000); `agent/events` reads a run's history from the table. The table isn't pruned yet (#187).

### Agents' environment (#96, minimal)

- Every process wispd starts loses the SSH session's variables, now including `SSH_AUTH_SOCK`: workers can't push, and wispd makes every commit locally.
- Agent CLIs, CLI detection, and worktree git commands share one environment: wispd's own, with `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, and the system folders appended to `PATH` where missing. Appending keeps the user's order; it only fills in what launchd or an SSH session left out.
- Capturing the login shell's `PATH` stays open on #96.

## Consequences

- The editor can drive a worker end to end: start, stream, message, cancel, list, and rebuild a transcript after a reconnect or a restart (#105, #157).
- A restart no longer changes `logId`. Tests that told wispd instances apart by `logId` use the serve pid.
- Codex and Cursor workers are refused until #122 and #123 set `worker_sandbox`.
- A worker's CLI that ignores `SIGINT` keeps a stopping wispd waiting for its cancel grace period, up to 15 s in total.
