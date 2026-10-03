# 0027: Every Claude thread uses Claude Code's permission modes

- Status: accepted; supersedes in part [0004](0004-subscription-providers.md) (the coordinator's no-write flags), [0013](0013-worker-sandbox.md) (a worker in Bypass Permissions runs without the sandbox), [0019](0019-coordinator-mcp-tools.md) (the coordinator's flags), and [0024](0024-coordinator-chat.md) (where the coordinator runs, its per-turn check, and its fixed permission); Manual, Auto, and Plan ask the app instead of denying what would prompt when the app asks for that ([0031](0031-permission-requests.md)); the coordinator's worktree is [0042](0042-project-children-are-threads.md)'s, and a Project's permission mode, Auto or Bypass, replaces [subagents inheriting the coordinator's mode](#subagents-inherit-the-coordinators-mode) (0042, PLX-394)
- Date: 2026-09-29
- Issue: RYA-188; RYA-249 for [the todo tools](#the-todo-tools); RYA-251 for the task list's `--settings`

## Context

Claude threads offered two access levels, Accept Edits and Plan (RYA-97). A project's coordinator offered none. It was locked to 0004's no-write flags: read tools, user settings only, no hooks, no MCP servers but plxd's, and `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB`. It also ran in a detached copy of the repository that plxd reset and write-checked around every turn (RYA-171). So the coordinator couldn't use the user's MCP servers, skills, plugins, or subagents, or run a command such as `gh`.

Ryan's decision: every Parallax thread works the way Claude Code does with permissions. That covers the coordinator, the subagents it spawns, and normal threads. Ryan runs Claude Code in bypass mode on his own machine, and Parallax runs the same CLI.

## Decision

### The modes

`AgentPermission` gains `auto`, `manual`, and `bypass`. The Claude backend reports all five, in Claude Code's order, and maps each to Claude Code's mode of the same name:

| Parallax | App label | `--permission-mode` |
| --- | --- | --- |
| `auto` | Auto | `auto` |
| `manual` | Manual | `default` |
| `edit` | Accept Edits | `acceptEdits` (the default) |
| `plan` | Plan | `plan` |
| `bypass` | Bypass Permissions | `bypassPermissions` |

Each backend reports its own list (`Backend::permissions`), and Codex's stays `edit` only. The composer's Access picker shows the thread's backend's list in every chat, coordinator chats included. A mode changes between turns, as model and effort do (RYA-161), and applies from the next CLI process.

Headless Claude Code can't ask anyone, so in Manual it denies every request that would prompt. Relaying those requests to the app is a follow-up, which [0031](0031-permission-requests.md) settles.

### The coordinator

- It is full Claude Code in its mode. Its only arguments beyond every run's are `--permission-mode <mode>`, `--mcp-config` with the `plxd mcp` server, `--allowedTools` with plxd's eight tools and then Claude Code's five todo tools ([The todo tools](#the-todo-tools), RYA-249), and `--settings` that only sets `CLAUDE_CODE_TASK_LIST_ID` empty, so it keeps its session's own task list ([0013](0013-worker-sandbox.md#claude-code), RYA-251). The allowlist lets plxd's tools work in every mode, including Manual and Accept Edits, which would otherwise deny them headless.
- User and project settings, hooks, skills, plugins, subagents, and MCP servers all load. The user's, the repository's, and plugins' MCP servers all connect.
- It runs without `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB`, which forces mode `default`. Its `system/init` must report the mode it asked for, or the run fails with `policyViolation`. That is how a scrub flag set by managed settings shows up. Its tools aren't checked.
- It runs in the project's repository, the user's own checkout, as Claude Code runs in the folder it was started in. It sees the user's uncommitted work, and its edits are real. The detached worktree and 0024's per-turn `git status` check are gone.
- `project/start` takes `permission`, and `agent/send` accepts one for a coordinator.

### Subagents inherit the coordinator's mode

`agent/start` with a `coordinatorThread` and no `permission` gives the new run the coordinator's current one, read from the coordinator's run when the spawn happens. A mode changed between the coordinator's turns applies to the subagents spawned after it. If the subagent's backend doesn't map the inherited mode (Codex, for example), the subagent gets that backend's default. A subagent that names its own mode keeps it.

### Bypass Permissions means no sandbox

Claude Code refuses `bypassPermissions` under `--restricted` ("bypassPermissions not supported in restricted mode", checked with 2.1.283). Ryan chose Claude Code's own behavior over keeping the sandbox, so a worker in Bypass Permissions runs as full Claude Code:

- It gets no `--restricted`, fixed `--tools`, `--strict-mcp-config`, or `worker_settings`, so it has no sandbox.
- It gets `--allowedTools` with Claude Code's five todo tools ([The todo tools](#the-todo-tools), RYA-249), and `--settings` that only sets `CLAUDE_CODE_TASK_LIST_ID` empty, as a coordinator does (RYA-251).
- It keeps its worktree, its temp folder, `--add-dir`, and the `PATH` file.
- Its `system/init` must still report its mode and a Claude Code of at least `WORKER_MIN_VERSION`, but its tools aren't checked.

Every other mode keeps 0013's sandbox unchanged. `--restricted` accepts `auto`, `default`, `acceptEdits`, and `plan`, checked with 2.1.283.

### The todo tools

Claude Code 2.1.283 offers its todo tools, `TodoWrite` or the four task tools that replace it (`TaskCreate`, `TaskGet`, `TaskList`, `TaskUpdate`), only to a built-in list of older models, or when `--tools` or `--allowedTools` names one of them, or with `CLAUDE_CODE_ENABLE_TODO_TOOLS` ([0013](0013-worker-sandbox.md#claude-code), RYA-248). A sandboxed worker's `--tools` names them. A coordinator and a bypass worker have no `--tools`, so on Opus 5.5, Fable 5.1, or Sonnet 5 they had no todo tool, and the app's plan card couldn't show for them (RYA-249). So both name the five tools in `--allowedTools`, a coordinator after plxd's eight. The CLI checks each allow rule's tool name for them as it checks `--tools`, so either flag turns them on.

- **What an allow rule adds.** `--allowedTools` also pre-approves what it names. For these tools that changes nothing. None defines its own permission check, and 2.1.283 ran them without asking in Manual, Auto, and Plan when only `CLAUDE_CODE_ENABLE_TODO_TOOLS` turned them on, with the prompt channel open ([0031](0031-permission-requests.md)) and no classifier request in Auto. They write only the session's own list in Claude Code's configuration folder, unless managed settings name a shared one in `CLAUDE_CODE_TASK_LIST_ID`. The user's and the project's settings can't, because the run's `--settings` sets it empty after them ([0013](0013-worker-sandbox.md#claude-code), RYA-251). A deny rule in the user's or the project's settings still wins: with `TaskCreate` denied, `system/init` doesn't list it.
- **`CLAUDE_CODE_ENABLE_TASKS`.** Naming either set turns on whichever set the variable picks. So a user who sets it to `false` gets `TodoWrite` back, as in their terminal, and the plan card reads `TodoWrite` too (RYA-248). plxd keeps that: a coordinator and a bypass worker are Claude Code as the user configured it. The allowlist names `TodoWrite` too, as a worker's `--tools` does, so its opt-in doesn't rest on the task tools' names. The variable reaches a run only through Claude Code's own settings. plxd's agent environment is an allowlist ([0014](0014-agent-runs.md)), so a value in the environment plxd started with never reaches a run, and plxd never sets it. A coordinator and a bypass worker read it from the user's or the project's settings `env`, and every run from the global config's (`.claude.json`, RYA-251) and from managed settings. A sandboxed worker reads no user or project settings (`--restricted`, 0013), and its `system/init` check accepts either set.
- **Other runs** keep their arguments: a sandboxed worker names the tools in `--tools` and gets no `--allowedTools`, and a plain no-write run keeps 0004's read tools, with no todo tool.

## Consequences

- A coordinator, and any thread in Bypass Permissions, can do anything Claude Code can on the user's machine. That includes reading `~/.ssh`, pushing, and running the repository's hooks and MCP servers, and wake-ups (0025) run turns when nobody is watching. Auto is the mode with a second check: a classifier judges each action.
- A bypass worker can write outside its worktree, including the user's checkout and other runs' worktrees. plxd still commits only what is in its worktree.
- A Manual worker reports `default`, which is also the mode the scrub flag forces, so its mode check can't catch the flag. On Linux, `linux_sandbox::check_host` still refuses a worker when the flag is on (RYA-112).
- Existing installs keep their old `coordinators/<project id>` worktrees, which plxd no longer uses. `git worktree remove` clears them.
- A plain no-write run, one with no coordinator tools, keeps 0004's flags and scrub.
- A coordinator and a bypass worker keep a plan with Claude Code's todo tools on every model, so the plan card shows for them (RYA-249).
- `Role::Coordinator` runs still have policy `noWrite`, which now only marks them as coordinator runs, not what they may do.

## Evidence

On 2026-09-29, on macOS 27.0 with Claude Code 2.1.283, each flag set below was run with `env -i`, a throwaway `CLAUDE_CONFIG_DIR` and git repository, a dummy `ANTHROPIC_API_KEY`, and `ANTHROPIC_BASE_URL=http://127.0.0.1:9`. `system/init` reported `permissionMode` before the first request:

| Flags | Result |
| --- | --- |
| `--permission-mode auto` | `auto` |
| `--restricted --permission-mode auto` | `auto` |
| `--restricted --permission-mode default` | `default` |
| `--restricted --permission-mode bypassPermissions` | exits: "bypassPermissions not supported in restricted mode" |

Not tested: a real turn in Auto, which needs the classifier and a live account.

On 2026-10-01, Claude Code 2.1.283 for Linux x64, the npm package whose binary has CI's pinned SHA-256, was run by hand as a coordinator and as a bypass worker with `--model claude-opus-5-5`, outside the built-in list, against a fake Messages API that asked for the task tools (RYA-249):

| Run | Result |
| --- | --- |
| A coordinator whose `--allowedTools` names only plxd's eight tools, as before RYA-249 | `system/init` lists no todo tool; `TaskCreate` fails with "No such tool available" |
| The same with the five todo tools after them | `system/init` lists the four task tools; `TaskCreate` answers `Task #1 created successfully: Add tests`, and the CLI writes `~/.claude/tasks/<session id>/1.json` |
| A bypass worker with no `--allowedTools`, and with the five todo tools | The same two results |
| A coordinator in Manual, Auto, and Plan with `--permission-prompt-tool stdio`, the tools turned on by `CLAUDE_CODE_ENABLE_TODO_TOOLS` and not in `--allowedTools` | `TaskCreate`, `TaskUpdate`, and `TaskList` run without asking; Auto sends no request beyond the turn's own |
| `CLAUDE_CODE_ENABLE_TASKS=false` in the user's `settings.json` `env` | `system/init` lists `TodoWrite` and not the task tools, whether or not `--allowedTools` names `TodoWrite` |
| `permissions.deny: ["TaskCreate"]` in the user's settings | `system/init` lists the other three task tools; `TaskCreate` fails with "No such tool available" |

2.1.283 refuses Bypass Permissions as root, and the container ran as root, so the bypass worker's runs set `IS_SANDBOX=1`. `daemon/src/backend/claude/fixtures/coordinator-tasks.jsonl` is the coordinator's transcript. `daemon/tests/permission_requests.rs` runs a Manual coordinator, with the prompt channel, and a bypass worker on `claude-opus-5-5` through plxd's backend with the pinned Claude Code on CI's Linux legs: `TaskCreate`, `TaskUpdate`, and `TaskList` answer without asking, and the list is in the configuration folder.
