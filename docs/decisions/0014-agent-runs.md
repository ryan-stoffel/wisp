# 0014: Agent runs in wispd

- Status: accepted; review and accept added by #157
- Date: 2026-09-25
- Issue: #156, #157 (review and accept), #68 (what Accept does)

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
- Events, all project-scoped and all carrying `runId`: `agent.started {run?}` (wispd always sends `run`; it is optional only because a v1 sample predicted `agent.started {runId}`), `agent.updated {state}` with only the fields that change during a run (status, account, session, error, diff, `updatedAt`; never the prompt), `agent.output {items}` coalesced per run every 50 ms, `agent.accountFallback`, `agent.finished {outcome}` once per CLI process, and `agent.diffReady {diff}` after each commit.
- Errors: `runNotFound`, `runNotResumable`, `workerUnavailable` (a backend without 0013, a missing or too old CLI with both versions named, a sandbox path holding `*?[]`), and `worktreeFailed` (for example a dirty repository).

### A run's life

- **Statuses:** `starting`, `running`, `completed`, `failed`, `cancelled`, `interrupted`. A run outlives its CLI processes: `completed`, `failed`, `cancelled`, and `interrupted` runs with a session id take `agent/send`.
- **Before anything is created,** wispd resolves the account (`routing::resolve`, `Role::Worker`), refuses a backend whose `Capabilities::worker_sandbox` is false, checks Claude Code's detected version against `WORKER_MIN_VERSION`, and canonicalizes and checks every sandbox path. Detection reads the version from `claude --version` when `claude auth status` doesn't report one.
- **Then** it creates the worktree, records the run and worktree rows in one transaction (with #166's pinned `git_dir`), and starts the CLI with `WorkerSandbox::for_worktree`, whose git folder comes from the user's own checkout. The first prompt tells the agent its limits (0013).
- **One actor task per run** takes commands and backend events in one loop. It charges usage to the run's current account, which changes on `AccountFallback` (0012).
- **When a CLI process ends,** wispd commits the worktree on the run's branch with `commit_all` (#166: pinned git folder, no hooks), whatever the outcome, and reports the commit's stats against the worktree's base. A failed commit fails the run with `commitFailed`.
- **When wispd stops,** it cancels running CLIs and records their runs `interrupted`, without committing. **When wispd starts,** any run still `starting` or `running` (a crash) becomes `interrupted`. Both resume through `agent/send` by their session id, on the account the session ended on (after any fallback), not the worker role's current default; if that account is gone, or now runs on another backend, `agent/send` fails with `runNotResumable`.

### Review and accept (#157, #68)

Added by #157, behind a new `agentReview` capability. What a client reviews is exactly what Accept merges: the run's latest commit (`run.diff.commit`) against its worktree's base. The uncommitted edits of a running agent can't be reviewed; wispd commits them when its CLI ends.

| Method | Params | Result |
| --- | --- | --- |
| `agent/diff` | `{runId}` | `{base, head, files, stats, truncated}`: per file its status, stats, and a unified diff capped at 256 KiB, with 4 MiB in all and 3,000 files listed |
| `agent/file` | `{runId, path, side: "base" \| "head"}` | `{commit, exists, size?, content?, tooLarge}`: base64 content, capped at 4 MiB |
| `agent/accept` | `{runId, id, commit?}` | `{run, merge: {commit, into, how: fastForward \| merge \| upToDate}}` |
| `agent/requestChanges` | `{runId, turnId, text}` | `{run}`, sent as `agent/send` sends a message |

- **Reads** come from git's objects (`ls-tree` and `cat-file` on the commit), through #166's pinned and hardened worktree calls, and never from the worktree's files. So a symlink comes back as its target text and is never followed, and a path through a symlinked folder doesn't exist. `path` must be relative, with no empty, `.`, or `..` component, no backslash or NUL, and nothing under `.git`, and it is matched literally.
- **Accept** (#68's default) merges the run's commit into the current branch of the project's checkout on the host, and never pushes. It refuses, with nothing changed (`mergeRefused`), when the run is running or has no commit, when HEAD is detached, when a merge, rebase, cherry-pick, revert, or am is in progress, when uncommitted changes (staged, unstaged, or untracked) touch a path the merge changes, or when `commit` isn't the run's latest commit. When the branch has moved on, `git merge-tree --write-tree` builds the merge in the object store, a conflict refuses with `mergeConflict` and names the files, and a clean result becomes an unsigned merge commit. The branch and working tree then move with `git merge --ff-only <result>`, which keeps unrelated uncommitted changes. Then wispd removes the worktree and branch, records the run `accepted`, and emits `agent.accepted {runId, merge}` and `agent.updated`. `id` makes a retry after a lost connection return the same answer, even after a restart. An accepted run fails `agent/diff`, `agent/file`, and `agent/send` with `runAccepted`.
- **The user's own git setup** applies to Accept's git calls, which run in the user's checkout: global config, filters such as Git LFS's, merge drivers, and identity. Hooks are the exception, turned off with `core.hooksPath=/dev/null`. The merge brings in files the worker wrote, and `core.hooksPath` often names a tracked folder (husky), so a `post-merge` or `post-checkout` hook could be the worker's own code, run unsandboxed in wispd as soon as it lands (0013: review is the last gate). Someone who relies on a post-merge hook runs it themselves.

### The event log is stored

0007 moved the log to SQLite in M3. The `events` table holds every event with its daemon-wide `seq`, its project, and its run. `log_meta` holds `logId`. So `logId` and `seq` now survive a restart, and an editor resubscribes from its last `seq` instead of reloading. `events/subscribe` still replays only from the newest `retention` events in memory (10,000); `agent/events` reads a run's history from the table. The table isn't pruned yet (#187). `agent/events` pages are capped at about 4 MiB of event JSON as well as by count, and always hold at least one event, so a page fits in 0007's 8 MiB frame. Replay tolerates gaps in `seq`; an event that fails to be stored is still delivered, and the stored `logId` is cleared so the next start gets a new one and clients resync.

### Agents' environment (#96, minimal)

- Agent CLIs, CLI detection, and worktree git commands share one environment, built from an allowlist of wispd's own: `PATH`, `HOME`, `USER`, `LOGNAME`, `SHELL`, `TMPDIR`, `LANG` and `LC_*`, `TERM`, `__CF_USER_TEXT_ENCODING`, the proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, `NO_PROXY`, in either case), and the CA variables (`SSL_CERT_FILE`, `SSL_CERT_DIR`, `NODE_EXTRA_CA_CERTS`). A worker has network access and reads prompt-injectable content, so a token wispd was started with (`GITHUB_TOKEN`, `NPM_TOKEN`, `OPENAI_API_KEY`, `AWS_*`, a database URL) must not reach it (0013). A backend adds what its account needs on top, and the launcher adds `WISPD_DATA_DIR`.
- Every process wispd starts also loses the SSH session's variables, including `SSH_AUTH_SOCK`: workers can't push, and wispd makes every commit locally.
- `PATH` gets `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, and the system folders appended where missing. Appending keeps the user's order; it only fills in what launchd or an SSH session left out.
- Capturing the login shell's `PATH` stays open on #96.

## Consequences

- The editor can drive a worker end to end: start, stream, message, cancel, list, and rebuild a transcript after a reconnect or a restart (#105, #157).
- A restart no longer changes `logId`. Tests that told wispd instances apart by `logId` use the serve pid.
- Codex and Cursor workers are refused until #122 and #123 set `worker_sandbox`.
- A worker's CLI that ignores `SIGINT` keeps a stopping wispd waiting for its cancel grace period, up to 15 s in total.
