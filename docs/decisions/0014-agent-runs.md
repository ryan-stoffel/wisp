# 0014: Agent runs in wispd

- Status: accepted; review and accept added by #157
- Date: 2026-09-25
- Issue: #156, #157 (review and accept), #68 (what Accept does), #191 (no hooks in the user's checkout)

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
| `agent/diff` | `{runId}` | `{base, head, files, stats, truncated}`: per file its status, stats, and a unified diff capped at 256 KiB, with 4 MiB in all, 2 MiB of file list, and 3,000 files listed. Caps count JSON-escaped size, so the answer fits 0007's 8 MiB frame |
| `agent/file` | `{runId, path, side: "base" \| "head", sizeOnly?}` | `{commit, exists, size?, content?, tooLarge}`: base64 content, capped at 4 MiB; `sizeOnly` leaves it out, for a file system's `stat` |
| `agent/accept` | `{runId, id, commit?}` | `{run, merge: {commit, into, how: fastForward \| merge \| upToDate}}` |
| `agent/requestChanges` | `{runId, turnId, text}` | `{run}`, sent as `agent/send` sends a message |

- **Reads** come from git's objects (`ls-tree` and `cat-file` on the commit), through #166's pinned and hardened worktree calls, and never from the worktree's files. So a symlink comes back as its target text and is never followed, and a path through a symlinked folder doesn't exist. `path` must be relative, with no empty, `.`, or `..` component, no backslash or NUL, and nothing under `.git`, and it is matched literally.
- **Accept** (#68's default) merges the run's commit into the current branch of the project's checkout on the host, and never pushes. It refuses, with nothing changed (`mergeRefused`), when the run is running or has no commit, when HEAD is detached, when a merge, rebase, cherry-pick, revert, or am is in progress, when uncommitted changes (staged, unstaged, or untracked) touch a path the merge changes, when any file, ignored ones included, sits where the merge adds one, or when `commit` isn't the run's latest commit. When the branch has moved on, `git merge-tree --write-tree` builds the merge in the object store, a conflict refuses with `mergeConflict` and names the files, and a clean result becomes an unsigned merge commit. The branch and working tree then move with `git merge --ff-only --no-overwrite-ignore <result>`, which keeps unrelated uncommitted changes and never overwrites an ignored file such as `.env`. Then wispd removes the worktree and branch, records the run `accepted`, and emits `agent.accepted {runId, merge}` and `agent.updated`. `id` makes a retry after a lost connection return the same answer, even after a restart. An accepted run fails `agent/diff`, `agent/file`, and `agent/send` with `runAccepted`.
- **The user's own git setup** applies to Accept's git calls, which run in the user's checkout: global config, filters such as Git LFS's, merge drivers, and identity. The merged commit's `.gitattributes` decides which files go through which of the user's filters, and `.lfsconfig` can name the LFS server, exactly as when the user runs `git merge` themselves. No code the agent wrote runs that way, only the user's own filter programs, so this is accepted.
- **A failed checkout puts back only what git wrote.** `merge --ff-only` can stop part way, when one of those filters fails (a required LFS smudge that can't download) or when git can't move the branch after checking out. Minutes can pass between the overlap check and that failure, and the user, an editor, or another git can write files meanwhile. So wispd never restores from a stale check:
  - When git failed on `index.lock`, another git holds the index and git wrote nothing, so wispd refuses and changes nothing.
  - When the 5-minute timeout stopped git, git leaves `index.lock` behind. wispd changes nothing and says so, naming the lock and the files that may be partly updated. It doesn't remove the lock, because it can't tell it from a live one.
  - Otherwise, for each path the merge changes, wispd compares the working tree (hashed as `git add` would) with the merge's version and HEAD's. A file that still holds exactly the merge's version is put back, index and working tree, with `git restore --source=HEAD --staged --worktree`, `.gitattributes` first. A file the merge added is removed, along with folders left empty. A file that holds HEAD's version needs nothing. A file that holds neither was written by someone else, so it is left alone and named in the error.
  - Refusing up front whenever `.gitattributes` changes was rejected: agents edit it legitimately, and a failing filter doesn't need an agent to be involved.
- **No git call wispd makes runs a repository hook.** Every call in the user's checkout, including Accept, `worktree add`, `worktree remove`, `branch -D`, and `worktree prune`, runs with `-c core.hooksPath=/dev/null`, like the worktree-scoped calls (#166). Once a run is accepted, the checkout's hooks can include files the worker wrote, because `core.hooksPath` often names a tracked folder (husky). A `post-merge`, `post-checkout`, or `reference-transaction` hook would otherwise run the worker's code unsandboxed in wispd: on the merge, on the branch deletion right after it, or on the next `agent/start` (0013: review is the last gate). Someone who relies on a hook, such as a post-merge install, runs it themselves.

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
