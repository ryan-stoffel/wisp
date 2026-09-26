# 0017: Normal threads are agent runs that belong to a repo entry

- Status: accepted
- Date: 2026-09-26
- Issue: #110

## Context

0011 put normal threads next to projects in wisp's sidebar: **Repositories**, with threads grouped under each repo, and **No Repo**, for quick chats. A normal thread is one agent conversation with no coordinator (0011, decision 5). #156 built the runner that subagents use: `agent/*`, a worktree per run, the worker sandbox (0013), commit on exit, and resuming through `agent/send` (0014). #105 built the editor side, where a run's transcript, composer, and Stop come from its events (0015). #110 had to decide how a thread maps onto wispd. The coordinator (#104, M4) and the review (#157) depend on the answer, because they read the same runs.

## Decision

### A thread is a run, and its scope is a repo entry

- **The runner is unchanged.** A thread's agent is an ordinary run: the same routing (0012), sandbox (0013), worktree, commit, `agent.*` events, `agent/send`, `agent/cancel`, `agent/events`, and resume after a restart (0014).
- **A repo entry** is a lightweight record of a repository on the host: `{id, name, path, scratch}`, registered by `repo/add` when the user picks the repository. It has no coordinator and no Project tab. One entry per canonical path, so asking twice for the same folder returns the first entry, whatever id the second request carried.
- **A run's scope.** `AgentRun.project`, the runs table's `project_id`, and the event log's project column hold a *scope id*: a project's id for a subagent, or a repo entry's id for a thread. So `agent/list {project}` and `events/subscribe {project}` take a repo entry's id as they take a project's, and a thread's `agent.*` events go to its entry's id. Both kinds of id are UUIDv7s, generated independently, so they never collide. The editor reuses #105's per-project code for repo entries unchanged.
- **Threads with no repo.** Each gets its own scratch repository at `<data folder>/scratch/<run id>`, which wispd makes with `git init`, a local `wisp <wisp@localhost>` identity, and one empty commit on `main`. The worktree is cut from it, as from any repository. Separate repositories keep one quick chat from reading another's files. All of them belong to one scratch entry (`scratch: true`, named "No Repo", with the `scratch` folder as its path), made on first use and announced with `repo.added`.
- **Shared context.** A thread writes notes to its entry's context folder, `context/<repo id>`, which the runner already gives every run. Threads in one repository share it. No UI shows it yet.
- **The first prompt** tells a thread's agent its limits, as a worker's does, without calling it a worker or mentioning a project.
- **The sandbox** (0013) is unchanged except that Claude's `allowRead` now also lists the read-only git paths: the worktree's `.git` file and the repository's git folder. A scratch repository's git folder lies inside wispd's data folder, which is `denyRead`. Without this, a thread with no repo couldn't run `git status` or `git diff`. For a repository outside the data folder, those paths were readable already, so nothing changes there. They stay `denyWrite`.

### Protocol, behind the `threads` capability

| Method | Params | Result |
| --- | --- | --- |
| `thread/list` | `{}` | `{repos, threads, seq}` |
| `repo/add` | `{id, path}` | `{repo}`; idempotent on `id`, and on `path` |
| `thread/start` | `{runId, repo?, prompt, account?}` | `{thread, run}`; without `repo`, a thread with no repo; idempotent on `runId` |
| `thread/archive` | `{runId, archived}` | `{thread}` |
| `thread/delete` | `{runId}` | `{}` |

- `Thread` is `{id, repo, archived?, createdAt}`, where `id` is the run's id. The run itself comes from `agent/list` and `agent.*`, as for a subagent.
- Host-level events: `repo.added {repo}`, `thread.started {thread}`, `thread.updated {thread}` (archive), and `thread.deleted {runId, repo}`.
- New error kinds: `repoNotFound`, `threadNotFound`, and `runActive`. `thread/delete` fails with `runActive` while the run's CLI runs, and the editor cancels the run first.
- `repo/add` takes an absolute path with no `.` or `..` segments that is the top folder of a git working tree (`notARepository` otherwise, as for `project/create`), and refuses wisp's own worktrees and scratch repositories.
- **Deleting** removes the thread, run, and worktree rows and the run's stored events in one transaction. It then removes the worktree and its branch, and a scratch repository after checking that its path is exactly `<scratch folder>/<run id>`. Startup's garbage collection removes a worktree folder that a crash left behind (#171).

### Store

Migration 9 adds `repos` and `threads`. Version 8 is #157's. Migrations now apply every version that isn't recorded, not only those above the newest, so two branches can each add one and land in either order.

### Editor

- `IWispThreadsService` keeps `thread/list` current from the host-level events. `IWispAgentsService` also follows each repo entry's runs, as it follows each project's.
- A thread is a `wisp.thread` session. Its main chat is its run's `wisp.agent` chat, with no tool origin, so #105's transcript, composer, and Stop work as they do for a subagent. A thread in a repository has that repository as its workspace. A thread with no repo is a quick chat, with no workspace.
- **New Chat** opens upstream's new-session composer. The provider offers `wisp.thread`, quick chats (the picker's "No workspace" choice starts a thread with no repo), and a browse action that picks a repository on the host.
- Archive, unarchive, and delete go through the provider to wispd.

## Rejected

| Option | Why not |
| --- | --- |
| Threads as projects, or repo entries in the projects table | `project/list` would return them, and an editor that doesn't know threads would list them under Projects with a coordinator. |
| A nullable project on runs, with thread events host-level | Every agent method and the editor's per-project code would need a second path for runs with no project. Host-level output would reach every subscriber. |
| One shared scratch repository for every thread with no repo | Each thread's worktree would share one object store and history, so one quick chat could read another's files. |
| Running a thread in the user's checkout instead of a worktree | Breaks 0013's contract. The user's own edits and the agent's would mix, and wispd couldn't commit the agent's work on its own branch. |

## Consequences

- Anything that lists runs without a project, such as `agent/list {}`, now returns threads' runs as well. Their `project` is a repo entry's id, which `project/list` doesn't know.
- The coordinator (M4) can later offer a thread's repository as a project without moving the thread.
- The Changes tab for a thread follows whatever #194 does for a subagent's `IChat.changes`.
- Nothing prunes threads or repo entries yet. Deleting is the user's, and #207 covers pruning runs.
