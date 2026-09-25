# 0013: The worker sandbox

- Status: accepted
- Date: 2026-09-25
- Issue: #137

## Context

M3 runs the first real workers: a vendor CLI in a git worktree with the `workspace-write` policy (0004). Until now that policy was Claude Code's `--permission-mode acceptEdits` alone, which is not a sandbox (#137):

- The read-only commands (`cat`, `grep`, `git log`, ...) run on any path. Every other command would prompt, and `-p` denies it, so a worker couldn't run `cargo test` or `npm test`.
- Worker runs loaded the project's settings. A repository's `env` block could redirect the credentials a run is billed to, or send an API key's traffic to another host (#134). Its hooks ran, and `-p` connected its `.mcp.json` servers without asking [3].

Codex and Cursor each have their own sandbox, and #156's runner needs one contract across all three. This record sets the contract and the flags that enforce it. Evidence is from the vendors' docs, read as raw Markdown on 2026-09-25, and from local experiments that need no vendor account (Evidence below). No real Claude run was made.

## Decision

### The contract

A worker is a vendor CLI running headless in its own worktree (#154). The same limits apply to every backend:

| | A worker may | A worker may not |
| --- | --- | --- |
| Write | Its worktree; the project's shared context folder (0005); its temp folder | Anything else. This includes the worktree's `.git` file and the repository's git folder, so wispd makes every commit (0004) |
| Read | The whole disk | Known credential stores (`WorkerSandbox`'s `UNREADABLE_IN_HOME`: `~/.ssh`, `~/.aws`, `~/.config/gh`, `~/.netrc`, the vendors' own config folders, shell history, `~/Library/Keychains`, ...) and wispd's data folder, except its own worktree and context folder |
| Execute | Any command, inside the vendor's OS sandbox (Seatbelt on macOS) | Anything outside it: no unsandboxed retries, no hooks, no MCP servers, no repository-supplied settings |
| Network | Nothing from its commands. The CLI's own model traffic is unaffected | Any host, including localhost. No web fetch or web search tools |

`WorkerSandbox` (`daemon/src/backend/sandbox.rs`) carries the paths. Every backend refuses a `workspace-write` run without one, with an error that names this record.

### Claude Code

Claude Code sandboxes Bash with Seatbelt on macOS. Its sandbox restricts writes to the working directories and the session temp folder, reads by `denyRead` rules, and network by a proxy with a domain allowlist [1]. A worker runs:

```sh
claude -p --output-format stream-json --verbose --input-format stream-json \
  --restricted \
  --tools Read,Edit,Write,Glob,Grep,NotebookEdit,Bash,TodoWrite \
  --strict-mcp-config \
  --permission-mode acceptEdits \
  --settings '<worker_settings>' \
  --add-dir <shared context folder> \
  [--model <m>] [--resume <id>]
```

`worker_settings` (`daemon/src/backend/claude.rs`) is:

```json
{
  "disableAllHooks": true,
  "sandbox": {
    "enabled": true,
    "failIfUnavailable": true,
    "autoAllowBashIfSandboxed": true,
    "allowUnsandboxedCommands": false,
    "excludedCommands": [],
    "network": { "strictAllowlist": true, "allowedDomains": [], "allowLocalBinding": false },
    "filesystem": {
      "denyRead": ["<unreadable paths>", "<the account's CLAUDE_CONFIG_DIR, if any>"],
      "allowRead": ["<worktree>", "<shared context folder>"],
      "denyWrite": ["<worktree>/.git", "<repository git folder>"]
    }
  }
}
```

- **`--restricted`** loads only managed settings and `--settings`. It skips the user, project, and local settings files, so a repository can't add allow rules, directories, hooks, or an `env` block. It also confines the file tools to the working directories, and it removes the command tools and WebFetch unless `--tools` names them [2]. It needs Claude Code 2.1.248 or later (`WORKER_MIN_VERSION`). We chose it over `--setting-sources user`, which would still merge the user's own sandbox arrays and allow rules into a worker's [1].
- **`--tools`** is an explicit list. `Bash` is on it without an allowlist, because the OS boundary holds whatever the command string says [1]. Argument patterns such as `Bash(npm test *)` are fragile by the vendor's own account [3], and the sandbox makes them unnecessary. The list leaves out web tools, `Agent`, `Skill`, `Monitor`, and every MCP tool. Leaving out `Skill` and `Agent` also means a repository's skills and subagents can't be invoked.
- **`failIfUnavailable`** makes a run fail when the sandbox can't start, instead of running commands unsandboxed. **`allowUnsandboxedCommands: false`** ignores `dangerouslyDisableSandbox`, the model's escape hatch [1][4].
- **`--strict-mcp-config`** with no `--mcp-config` connects no MCP servers, including `.mcp.json` [2]. wispd's own MCP tools join in M4.
- **`acceptEdits`** approves the file tools inside the working directories. Writes to protected paths (`.git`, `.claude`, `.vscode`, `.husky`, `.mcp.json`, shell startup files, ...) still prompt, and `-p` denies them [5]. The sandbox protects the same kind of paths from commands [1].
- **`denyWrite` on git metadata.** In a linked worktree the sandbox would otherwise let commands write the repository's shared git folder, for `git commit` [1]. That would let a worker move any branch, including the user's. wispd commits for every backend instead.
- As a second check, a worker's `system/init` must list only these tools. Otherwise the run fails with `policyViolation`, as a no-write run does with anything outside its read tools.

**#134, the project `env` gap.** For workers it is closed: `--restricted` reads no project or user settings file, so no repository `env` block reaches the CLI. The existing checks (`apiKeySource` in `system/init`, `modelUsage[*].provider` in `result`) stay as a second line. Managed settings and wispd's scrubbed environment are the only remaining sources.

**What a repository can still change for a Claude worker.** Only its `CLAUDE.md`, which is instructions and not configuration: it still loads. Everything else a repository supplies is either not loaded or is used only through a tool the worker doesn't have.

### Codex (for #122)

Codex sandboxes commands with Seatbelt. Its `:workspace` permission profile writes the workspace roots and the temp folders, protects `.git` (including the folder a `.git` file points to) and `.codex`, and has no network [6][7]. Permission profiles, which are in beta, can also deny reads [7]. A worker runs:

```sh
codex exec --json -C <worktree> \
  --ignore-user-config --ignore-rules \
  -c 'default_permissions="wisp_worker"' \
  -c 'permissions.wisp_worker.extends=":workspace"' \
  -c 'permissions.wisp_worker.workspace_roots={"<shared context folder>"=true}' \
  -c 'permissions.wisp_worker.filesystem={"<unreadable path>"="deny", ...}' \
  -c 'approval_policy="never"' \
  -c 'web_search="disabled"' \
  -c 'projects."<worktree>".trust_level="untrusted"' \
  -c 'shell_environment_policy.ignore_default_excludes=false'
```

- Profiles and `sandbox_mode` don't compose: passing `-s` or loading a `sandbox_mode` from any config disables the profile [7]. So #122 passes no `-s`, and `--ignore-user-config` keeps the user's `sandbox_mode` out. Auth still comes from `CODEX_HOME` [8].
- An untrusted project skips the repository's `.codex/` config, hooks, and rules [9]. `approval_policy="never"` is explicit, because an untrusted project otherwise asks for approval, and exec denies approval requests (0004).
- `.git` is read-only, so wispd commits (0004).
- If #122 finds that permission profiles misbehave on the version it pins, the fallback is `-s workspace-write --add-dir <context>` with `sandbox_workspace_write.network_access=false`. That fallback loses the read denials, and #122 records the gap.

### Cursor (for #123, still gated on #35)

`agent -p --output-format stream-json --trust --workspace <worktree> --add-dir <context> --sandbox enabled`, never `--force`, `--yolo`, or `--approve-mcps` [10][11]. `agent sandbox run` shows the policy: `workspace_readwrite`, reads bounded only by `system`, network denied by default. Its flags can't deny reads (Evidence). #123 must find a way to hide credential stores, or record the gap and have Ryan accept it.

### Tests and builds

A worker runs a project's tests with its shell tool, inside the vendor sandbox, like any other command. Build output goes in the worktree (`target/`, `node_modules/`, `dist/`), and toolchains and package caches under the home folder are read. In local experiments (Evidence), `cargo test --offline` with dependencies already in `~/.cargo` and `npm test` both passed under Codex's sandbox, including this repository's own `wisp-protocol` suite (56 tests, built from scratch in the sandbox).

What doesn't work in v1:

- **Installing dependencies.** A fresh worktree has no `node_modules`, and commands have no network. #167 adds a per-project setup command that wispd runs in the worktree before the worker starts. It runs with network, under wisp's own `sandbox-exec` profile, not the vendor's.
- **Tests that bind localhost or use Unix sockets.** All three sandboxes block both by default, and so do wispd's own server tests. #168 decides the settings (Claude `allowLocalBinding` and `allowUnixSockets`, Codex `--allow-unix-socket`, ...).
- **Docker, Apple Events, and the Keychain.** The Keychain was unreachable from commands in all three vendor sandboxes (Evidence).

### No OS layer of wisp's own around the CLI, in v1

Wrapping the whole CLI in a wisp `sandbox-exec` profile was tried and rejected:

- **The vendors' sandboxes stop working.** Under any restrictive outer Seatbelt profile, even one that only denies one read, a nested `sandbox_apply` fails with `Operation not permitted` (Evidence). Claude Code would then refuse to start under `failIfUnavailable`, and Codex's commands would fail, so wisp would be replacing the vendor sandbox rather than adding to it.
- **It can't filter the network by host.** The CLI needs its model API, and Seatbelt filters by address and port, so an outer profile can't allow the model while blocking everything else.
- **It would have to allow the vendors' state writes.** Session files, `~/.claude`, `~/.codex`, and token refreshes in the Keychain all need write access, which opens exactly the files a sandboxed command must not touch.

A separate macOS user was rejected too. The user's own signed-in CLI and its Keychain entries belong to the user's account (0004); a second user needs admin setup, its own vendor login, and `safe.directory` exceptions for the repository.

wisp's own profile does have one use: wispd runs commands itself, such as #167's setup step. Nothing nests inside those, and they need no domain filtering beyond on or off.

### What #156's runner does

#156's comment spells out the exact values:

1. Starts a worker only on a backend that implements this record: Claude now, Codex and Cursor once #122 and #123 do.
2. Before starting a Claude worker, checks the detected version (#114) against `WORKER_MIN_VERSION`, and refuses with an error that names both versions.
3. Passes `sandbox: Some(WorkerSandbox::for_worktree(home, data_dir, worktree, git_common_dir, context_dir))`, with every path canonical. Seatbelt matches real paths, and `/var` and `/tmp` are symlinks on macOS.
4. Commits the worktree's changes itself, with the git folder pinned and hooks off (#166). A worker can write files that git hooks run, such as `.husky/*` under `core.hooksPath`, and `--no-verify` skips only `pre-commit` and `commit-msg` [12]. Cursor's sandbox also leaves the worktree's `.git` file writable, and that file says which repository git uses.

## Deferred, and asked of Ryan on #137

- **Network for commands.** v1: none. Asked: allow package registries only? That lets agents add dependencies, but domain fronting can get past a host allowlist [1].
- **Web tools.** v1: none for workers. Asked: allow web search?
- **Read scope.** v1: a denylist of known credential stores. Asked: deny the whole home folder except toolchains instead? That is safer, but it breaks tools installed under `~` unless each one is listed.
- Localhost and Unix sockets (#168), the setup step (#167), and a check against a real Claude login (#124).

## Consequences

- Workers can run tests and builds that need no network. That is enough for M3's single-agent task and for Rust projects, but not for JavaScript projects in a fresh worktree until #167 lands.
- A worker's own shell can't commit, push, or reach any host, and an agent that expects to will see its command fail. Its prompt (#156) should say so.
- The worker contract depends on vendor flags that change often: `--restricted` is weeks old, and Codex's permission profiles are in beta. Each adapter pins a tested CLI version (0004), and CI's argv tests pin the flags.
- The denylist can't cover every secret. Anything outside it is readable, and whatever the agent reads can reach the vendor in the transcript.
- Reviewing the diff is the last gate. A worker can change files that run later outside any sandbox, such as `package.json` scripts, `Makefile`, or `.vscode/tasks.json` (the Edit tool refuses `.vscode`, but a Codex or Cursor command doesn't).

## Evidence

Local experiments on macOS 27.0 with Codex CLI 0.154.0 (`codex sandbox -P <profile>`), Cursor CLI 2026.09.10 (`agent sandbox run`), and `sandbox-exec`. None needs a vendor login. A probe script tried each operation from a simulated linked worktree whose `.git` file points to a separate git folder, with the context folder, the git folder, and a "secret" all outside the worktree and outside `/tmp`. Claude Code's sandbox needs a signed-in session, so its column comes from the docs [1] and is left for #124 to confirm.

| Operation | Codex `:workspace` | Codex `wisp_worker` profile | Cursor sandbox | wisp `sandbox-exec` profile |
| --- | --- | --- | --- | --- |
| Write the worktree | allowed | allowed | allowed | allowed |
| Write the context folder | denied (not a root) | allowed | allowed (`--allow-paths`) | allowed |
| Write `$TMPDIR` / `/tmp` | allowed / allowed | allowed / allowed | denied / allowed | allowed / denied |
| Write elsewhere, or through a symlink in the worktree | denied | denied | denied | denied |
| Write the worktree's `.git` file | denied | denied | **allowed** | denied |
| Write the repository's git folder | denied | denied | denied | denied |
| Read a file outside the roots | allowed | denied (`deny` rule) | allowed | denied |
| Read it through a hard link made in the worktree | denied | denied | denied | denied |
| HTTPS to example.com | denied | denied | denied | denied |
| Bind 127.0.0.1 / a Unix socket in the worktree | denied / denied | denied / denied | denied / denied | allowed / allowed |
| Search the Keychain with `security` | denied | denied | denied | allowed |
| Start a nested `sandbox-exec` | denied | denied | denied | denied |

Nesting: `sandbox-exec` inside `sandbox-exec` works only when the outer profile is `(allow default)` with nothing denied. With a single `deny` of writes, reads, or network, the inner `sandbox_apply` fails with `Operation not permitted` (exit 71). `codex sandbox` inside wisp's profile failed the same way.

## Sources

Read on 2026-09-25, as raw Markdown (`.md` appended to each page URL).

1. Claude Code, sandboxing (filesystem and network isolation, protected paths, git worktrees, `failIfUnavailable`, `allowUnsandboxedCommands`, security limitations): https://code.claude.com/docs/en/sandboxing
2. Claude Code CLI reference (`--restricted`, `--tools`, `--add-dir`, `--strict-mcp-config`, `--settings`): https://code.claude.com/docs/en/cli-reference
3. Claude Code permissions (Bash rule limits, working directories, what runs before you trust a folder): https://code.claude.com/docs/en/permissions
4. Claude Code settings reference (`sandbox.*`, `permissions.*`, `disableAllHooks`): https://code.claude.com/docs/en/settings-reference
5. Claude Code permission modes (`acceptEdits`, protected paths): https://code.claude.com/docs/en/permission-modes
6. Codex, agent approvals and security (network, protected paths in writable roots, `codex sandbox`): https://learn.chatgpt.com/docs/agent-approvals-security
7. Codex permission profiles: https://learn.chatgpt.com/docs/permissions
8. Codex CLI 0.154.0 `codex exec --help` (`--ignore-user-config`, `--ignore-rules`, `--add-dir`)
9. Codex configuration reference (`projects.<path>.trust_level`, `sandbox_workspace_write.*`, `shell_environment_policy.*`): https://learn.chatgpt.com/docs/config-file/config-reference
10. Cursor CLI parameters (`--sandbox`, `agent sandbox run`): https://cursor.com/docs/cli/reference/parameters
11. Cursor CLI configuration (`sandbox.mode`, `sandbox.networkAccess`, project `.cursor/cli.json`): https://cursor.com/docs/cli/reference/configuration
12. Git, `git commit --no-verify` and githooks: https://git-scm.com/docs/git-commit, https://git-scm.com/docs/githooks
