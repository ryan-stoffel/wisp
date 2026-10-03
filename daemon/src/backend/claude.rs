//! The Claude Code backend: runs the user's own signed-in `claude` CLI headless (0004, #116).
//!
//! # The command
//!
//! Every run is `claude -p --output-format stream-json --verbose --input-format stream-json` in
//! the run's cwd, plus the policy's flags, `--model`, `--effort`, and `--resume <session id>`
//! (0004 [10]), with `--fork-session` for a fork's first run (0050). Fast mode is `fastMode` in the run's one `--settings`, and a 200k context window
//! is [`DISABLE_1M_ENV`]:
//!
//! - **No-write** is 0004's: [`NO_WRITE_ARGS`], then [`no_write_settings`] as `--settings`, which
//!   also keeps the file tools out of Claude Code's shared temp folder (RYA-176). As a second
//!   check, a no-write run whose `system/init` lists any tool outside [`NO_WRITE_TOOLS`] fails with
//!   [`FailureKind::PolicyViolation`].
//! - **A coordinator**, a no-write run with plxd's own MCP tools attached (0019), is full Claude
//!   Code instead (0027): the run's [`permission_mode`], then `--mcp-config` with the `plxd mcp`
//!   server, which joins the user's, the repository's, and plugins' servers, and `--allowedTools`
//!   with [`crate::mcp::ALLOWED_TOOLS`], so plxd's tools work in every mode, and [`TODO_TOOLS`],
//!   so it keeps a plan on every model (RYA-249), then `--settings` with only [`settings_env`].
//!   Its user and project settings, hooks, skills, plugins, and subagents all load, as in a
//!   terminal. As a second check, a coordinator whose `system/init` reports another permission
//!   mode fails with [`FailureKind::PolicyViolation`].
//! - **Workspace-write** is 0013's worker sandbox: [`WORKSPACE_WRITE_ARGS`], then the run's
//!   [`permission_mode`], then [`worker_settings`] as `--settings`, then `--add-dir` for each
//!   writable folder. A normal thread in any mode whose client answers permission requests (0034),
//!   and a worker in [`AgentPermission::Bypass`] (0027), are full Claude Code instead, as on the
//!   user's own machine ([`unsandboxed`]): only the permission mode, `--allowedTools` with
//!   [`TODO_TOOLS`], `--add-dir`, and `--settings` with only [`settings_env`], with no sandbox, so
//!   the user's settings, `CLAUDE.md` files, skills, plugins, hooks, subagents, and MCP servers all
//!   load, and its `system/init` may list any tool. Otherwise:
//!   - `--restricted` loads no user, project, or local settings files, so a repository's
//!     `.claude/settings.json` can't add allow rules, hooks, or an `env` block (#134), and it
//!     confines the file tools to the working directories.
//!   - `--tools` names exactly [`WORKER_TOOLS`]. `Bash` is among them because Claude Code's own
//!     sandbox (Seatbelt on macOS, bubblewrap on Linux) holds every command: writes only to the
//!     working directories and the run's temp folder, no reads of the sandbox's `unreadable`
//!     paths, and no writes to git metadata. `failIfUnavailable` and
//!     `allowUnsandboxedCommands: false` keep a command from ever running outside it. Commands,
//!     `WebFetch`, and `WebSearch` reach any host but [`WORKER_DENIED_HOSTS`] (Ryan, #137), so
//!     the unreadable paths are what keep secrets in. A worker in Plan that asks plxd also gets
//!     `ExitPlanMode`, to hand its plan over ([`hands_over_plans`]).
//!   - `--strict-mcp-config` connects no MCP servers, including the repository's `.mcp.json`.
//!
//!   As a second check, a worker whose `system/init` lists a tool outside [`WORKER_TOOLS`]
//!   (and `ExitPlanMode` for one that hands over plans), reports a Claude Code older than
//!   [`WORKER_MIN_VERSION`], or reports a permission mode other than the one it asked for
//!   ([`permission_mode`]), fails with [`FailureKind::PolicyViolation`].
//!
//!   Only macOS and Linux run workers, with the same settings. On Linux,
//!   `linux_sandbox::check_host` checks before each worker that the sandbox works, seccomp
//!   filter included, because `failIfUnavailable` doesn't cover the filter (0013). It also
//!   refuses a worker when Claude Code runs with [`SCRUB_ENV`] on, which managed settings can
//!   set (RYA-112). That check runs in a separate process, so the permission mode check, which
//!   the flag fails, backs it up from inside the worker's own (RYA-118). Elsewhere the backend
//!   reports no `worker_sandbox` and refuses a workspace-write run.
//!
//! # A worker's temp folder
//!
//! Claude Code keeps its temp files in `$CLAUDE_CODE_TMPDIR/claude-<uid>`, `/tmp/claude-<uid>` by
//! default, which every Claude Code session of the user shares, and its sandbox lets commands
//! write there. So a worker's CLI gets [`TEMP_ENV`] set to the run's own folder,
//! [`WorkerSandbox::temp`] (RYA-130), and its commands get `<temp>/claude-<uid>` as their
//! `TMPDIR`. Claude Code does that only while the path fits in [`MAX_COMMAND_TEMP_BYTES`], and
//! falls back to the shared folder otherwise, so a longer one refuses the worker
//! ([`worker_temp`]). The rest of the run's folder stays hidden from commands, and the settings
//! take back the paths the sandbox always lets them write ([`WORKER_DENIED_WRITES`]).
//!
//! The CLI's own `TMPDIR` stays plxd's. Claude Code keeps its sandbox's Linux proxy bridges
//! there, which commands must reach, and Node's compile cache, which they must not write. In the
//! run's folder the first would be hidden and cut commands off the network (RYA-107).
//!
//! # A worker's `PATH`
//!
//! Claude Code runs each Bash command through the user's `$SHELL`, and zsh reads `/etc/zshenv`
//! and `~/.zshenv` for every command, so startup files that set `PATH` outright replace the
//! `PATH` plxd gave the CLI. Claude Code's shell snapshot would put it back, but the snapshot
//! sits in the configuration folder, which a worker's commands can't read (RYA-126). So a worker
//! also gets [`ENV_FILE_ENV`]: [`write_env_file`] writes a script into the data folder's `tmp/`
//! that puts the CLI's `PATH` back in front, which the CLI reads itself and runs before each
//! command. The run's driver deletes it once the CLI has exited.
//!
//! # Messages go on stdin
//!
//! With `--input-format stream-json`, the prompt and every follow-up are user messages on stdin,
//! one JSON object per line, as the Agent SDK sends them, with a message's images as base64 image
//! blocks before its text (RYA-191). The prompt never goes in argv, where
//! `ps` would show it and `ARG_MAX` would limit it. Each message carries a `uuid`, the turn id,
//! which the CLI echoes in `result.user_message_uuids`: several messages sent close together can
//! run as one turn, and those ids say which turns a result ended. Once no turn is outstanding,
//! stdin closes and the CLI exits after its last result, which ends the run; a follow-up sent
//! after that fails with [`SendError::Finished`](super::SendError::Finished).
//!
//! # Credentials
//!
//! Every run, whatever its account, drops each inherited variable that could choose Claude's
//! credentials, provider, or endpoint: names starting with one of [`SCRUBBED_PREFIXES`], plus
//! [`SCRUBBED_VARS`]. Those include the three that outrank the login (0004), the cloud provider
//! switches, `ANTHROPIC_BASE_URL`, which would send the login's token elsewhere, and the profile
//! and federation variables. `CLAUDE_CONFIG_DIR` is dropped too, and set only to the account's
//! own configuration folder. [`apply_credential`] then injects only what the account needs: the
//! account's configuration folder for a subscription, or, for an API key account (#118), only
//! [`API_KEY_ENV`] with the key [`key_account::resolve`](super::key_account::resolve) read from
//! the Keychain. The key is never in `args`, so `ps` can't show it, and every copy of it plxd
//! makes along the way ([`super::ApiKey`]'s own buffer, [`super::process::Environment`]'s
//! entries, and the buffers `spawn_session` builds from them) is zeroized once it is done with
//! it. No run inherits [`SCRUB_ENV`]; a no-write run other than a coordinator sets it, so the
//! CLI's own subprocesses don't get the key. A worker's sandbox withholds [`WORKER_WITHHELD_VARS`] from its sandboxed
//! commands only: the helpers Claude Code runs outside the sandbox, such as `git` and `rg`, still
//! inherit it.
//!
//! A project's `env` block can still set variables for a worker, and a coordinator or a bypass
//! worker loads the repository's settings (0004, #134, 0027), so the output is
//! checked as well. A `system/init` whose `apiKeySource` isn't the account's, or is missing, and
//! a `result` whose `modelUsage` names a provider other than `firstParty`, kill the CLI's process
//! group at once and fail the run with [`FailureKind::UnexpectedApiKey`].
//!
//! # The task list
//!
//! Claude Code's task tools keep a session's list under its session id, unless [`TASK_LIST_ENV`]
//! names a list that other sessions share (RYA-251). No run inherits it ([`SCRUBBED_VARS`]), but
//! the CLI also copies settings `env` blocks into its own process: its global config's, which
//! even `--restricted` reads, and those of the user, project, and local settings that a
//! coordinator and a bypass worker load. So every run's `--settings`, which the CLI applies after
//! them, sets it empty ([`settings_env`]). Managed settings are applied last, so their `env`
//! still picks the list for every run (0013).
//!
//! # Permission requests
//!
//! In Manual, Auto, and Plan, a worker's, a thread's, or a coordinator's CLI, and a thread's in
//! Accept Edits too, gets [`PROMPT_TOOL_ARGS`], as the Agent SDK passes them for its
//! `canUseTool` (RYA-222, 0031), when its client answers permission requests
//! ([`RunRequest::approvals`]). Instead of denying a tool call nobody approved, the CLI writes a
//! `can_use_tool` control request on stdout and waits. The driver reports it as
//! [`Event::ApprovalRequested`] and writes the answer that [`Run::answer`] gives as a
//! `control_response` on stdin, which stays open while a request waits. A
//! `control_cancel_request` withdraws one, as the CLI's exit withdraws every one left, and any
//! other control request gets an error response. A sandboxed worker in Accept Edits and every run
//! in Bypass Permissions never ask, a plain no-write run denies what isn't allowed (`dontAsk`),
//! and a run without `approvals` denies what would prompt, so their CLIs run as before. In Plan,
//! the plan itself is a request: `ExitPlanMode`'s, which a coordinator and a thread always have
//! with the channel and a worker gets with it (RYA-243).
//!
//! # Cancel
//!
//! `SIGINT` ends Claude's turn, while `SIGTERM` leaves it unfinished (0004 [11]), so cancel sends
//! `SIGINT`, closes stdin so the CLI exits after the interrupted turn, and kills the process
//! group if it is still running after the grace period.

#[cfg(target_os = "linux")]
pub mod linux_sandbox;
mod stream;
#[cfg(all(test, unix))]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempPath;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Notify, mpsc};

pub(crate) use self::stream::micros as usd_micros;
pub(crate) use self::stream::version as parse_version;
use self::stream::{Ask, Step, Translator, TurnDone};
use super::commands::{self, CommandsProbe};
use super::event::{Event, Failure, FailureKind, Outcome, WarningKind};
use super::process::{
    CancelPolicy, Environment, Exit, Launcher, Output, OutputLimits, Process, ProcessSpec, Signal,
    SpawnError, StdinMode, StdinPipe,
};
use super::sandbox::worker_sandbox;
use super::{
    AgentEffort, AgentPermission, Answer, AnswerError, ApprovalId, Backend, CancelSwitch,
    Capabilities, Credential, Decision, EVENT_BUFFER, EventSink, FollowUp, PromptImage, Run,
    RunHandle, RunId, RunRequest, SendError, StartError, Started, ToolPolicy, TurnId,
    WorkerSandbox, check_argument, prepend_path_line,
};
use crate::mcp;

/// The CLI's program name, looked up on the launcher's `PATH`.
pub const PROGRAM: &str = "claude";

/// The arguments every run starts with.
pub const BASE_ARGS: &[&str] = &[
    "-p",
    "--output-format",
    "stream-json",
    "--verbose",
    "--input-format",
    "stream-json",
];

/// [`ToolPolicy::NoWrite`]'s fixed arguments for a run that isn't a coordinator, as 0004 has
/// them: read-only built-in tools, only the user's settings (so no project `env` block or hooks),
/// no MCP servers, and every call that would prompt denied. [`arguments`] adds
/// [`no_write_settings`] after them.
pub const NO_WRITE_ARGS: &[&str] = &[
    "--tools",
    "Read,Glob,Grep",
    "--setting-sources",
    "user",
    "--strict-mcp-config",
    "--permission-mode",
    "dontAsk",
];

/// The `--settings` a no-write run other than a coordinator gets: hooks off (0004), and no `Read`
/// under Claude Code's shared temp folder, `/tmp/claude-<uid>` in both spellings
/// ([`commands_temp`]), which holds every session's files and which Claude Code otherwise lets it
/// read outside its cwd (RYA-176). 0013 hides the same folder from workers. A `Read` rule covers
/// `Glob` and `Grep` too. The folder is always in `/tmp`, because no no-write run gets
/// [`TEMP_ENV`]: every agent CLI starts from plxd's allowlisted environment
/// (`agents::worker::agent_environment`, 0014), which drops an inherited one, and only a worker
/// has one injected. Like every run's, it holds [`settings_env`].
#[must_use]
pub fn no_write_settings() -> Value {
    let deny: Vec<String> = ["/tmp", "/private/tmp"]
        .into_iter()
        .map(|temp| format!("Read(/{}/**)", commands_temp(Path::new(temp)).display()))
        .collect();
    serde_json::json!({
        "disableAllHooks": true,
        "permissions": {"deny": deny},
        "env": settings_env(),
    })
}

/// The `env` in every run's `--settings` (RYA-251): [`TASK_LIST_ENV`] empty, which Claude Code
/// 2.1.283 treats as unset, so the run keeps its session's own task list. The CLI copies into its
/// own process the `env` of its global config (`.claude.json` in the configuration folder, which
/// even `--restricted` reads), then of the user's, the project's, and the local settings it loads,
/// then of `--settings`, then of managed settings. So this beats every one but managed settings,
/// whose `env` still picks the list for every run (0013). The CLI keeps only the last
/// `--settings`, so each run gets one, with this in it.
#[must_use]
pub fn settings_env() -> Value {
    serde_json::json!({ TASK_LIST_ENV: "" })
}

/// The only built-in tools a no-write run's `system/init` may list. `EndConversation` stays
/// whatever `--tools` says (the CLI reference), and only ends the session. A coordinator's
/// `system/init` may list any tool: it is full Claude Code (0027).
pub const NO_WRITE_TOOLS: &[&str] = &["Read", "Glob", "Grep", "EndConversation"];

/// The built-in tools a worker gets (0013): the file tools, `Bash`, which Claude Code's sandbox
/// confines, the web tools (Ryan, #137), and the todo tools. No subagents, skills, or MCP tools.
/// `EndConversation` may appear in `system/init` as well, as for a no-write run. A worker that
/// [`hands_over_plans`] gets `ExitPlanMode` too ([`PLAN_WORKER_TOOL_LIST`]).
///
/// The todo tools are `TodoWrite` and the four task tools that replace it in Claude Code 2.1.283,
/// which offers one set or the other, never both: the task tools unless `CLAUDE_CODE_ENABLE_TASKS`
/// is `false` (RYA-248). Naming either set in `--tools` also turns them on for models Claude Code
/// would otherwise give none. The task tools keep the session's list in the CLI's configuration
/// folder, which the CLI writes itself and the worker's commands can't read (0013).
pub const WORKER_TOOLS: &[&str] = &[
    "Read",
    "Edit",
    "Write",
    "Glob",
    "Grep",
    "NotebookEdit",
    "Bash",
    "WebFetch",
    "WebSearch",
    "TodoWrite",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskUpdate",
];

/// The todo tools, the end of [`WORKER_TOOLS`]: `TodoWrite` and Claude Code's four task tools. A
/// coordinator and a worker in [`AgentPermission::Bypass`] have no `--tools`, so they name these
/// in `--allowedTools` instead, which turns them on in the same way (RYA-249). Whichever set
/// `CLAUDE_CODE_ENABLE_TASKS` picks is then on, so a user who turned the task tools off in their
/// settings' `env` gets `TodoWrite` back, as in their terminal. An allow rule also pre-approves a
/// tool, which changes nothing for these: Claude Code 2.1.283 runs them without asking in every
/// mode. A deny rule in the user's settings still wins (0027).
pub const TODO_TOOLS: &[&str] = &[
    "TodoWrite",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskUpdate",
];

/// [`WORKER_TOOLS`] as `--tools` takes them.
pub const WORKER_TOOL_LIST: &str = "Read,Edit,Write,Glob,Grep,NotebookEdit,Bash,WebFetch,WebSearch,\
     TodoWrite,TaskCreate,TaskGet,TaskList,TaskUpdate";

/// [`WORKER_TOOL_LIST`] and `ExitPlanMode`, for a worker that [`hands_over_plans`] (RYA-243).
pub const PLAN_WORKER_TOOL_LIST: &str = "Read,Edit,Write,Glob,Grep,NotebookEdit,Bash,WebFetch,\
     WebSearch,TodoWrite,TaskCreate,TaskGet,TaskList,TaskUpdate,ExitPlanMode";

/// The names for this Mac that no worker command or `WebFetch` may reach, even with network
/// access: this Mac's own services wait on #168. The sandbox's proxy canonicalizes other
/// spellings of loopback (`127.1`, `[::ffff:127.0.0.1]`) and refuses names that resolve to this
/// Mac, but it doesn't check IP literals, so the unspecified addresses are listed too. This Mac's
/// interface addresses aren't: 0013 records that gap.
pub const WORKER_DENIED_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]", "0.0.0.0", "[::]"];

/// The permission mode a worker or a coordinator asks for by default ([`permission_mode`]). It
/// must then report the mode it asked for in `system/init`. Claude Code forces `default` instead when
/// [`SCRUB_ENV`] is on, so another mode there means the worker's own process runs in scrub mode,
/// whatever `linux_sandbox::check_host` saw (RYA-118).
pub const DEFAULT_PERMISSION_MODE: &str = "acceptEdits";

/// [`AgentPermission::Bypass`]'s mode. Claude Code refuses it under `--restricted`, so a worker
/// in it runs without the worker sandbox (0027).
pub const BYPASS_PERMISSION_MODE: &str = "bypassPermissions";

/// The arguments, after `--permission-mode`, that make a run's CLI ask plxd over stdio before a
/// tool call that would prompt, instead of denying it (RYA-222, 0031). Only runs that
/// [`prompts`] get them.
pub const PROMPT_TOOL_ARGS: &[&str] = &["--permission-prompt-tool", "stdio"];

/// The tool whose approval takes Claude Code out of plan mode.
const EXIT_PLAN_MODE: &str = "ExitPlanMode";

/// [`ToolPolicy::WorkspaceWrite`]'s fixed arguments (0013), in every mode but
/// [`AgentPermission::Bypass`]. [`arguments`] adds the run's `--permission-mode`
/// ([`permission_mode`]), [`worker_settings`], and `--add-dir` folders after them.
pub const WORKSPACE_WRITE_ARGS: &[&str] = &[
    "--restricted",
    "--tools",
    WORKER_TOOL_LIST,
    "--strict-mcp-config",
];

/// [`WORKSPACE_WRITE_ARGS`] with [`PLAN_WORKER_TOOL_LIST`] as `--tools`, for a worker that
/// [`hands_over_plans`] (RYA-243). Nothing else differs.
pub const PLAN_WORKSPACE_WRITE_ARGS: &[&str] = &[
    "--restricted",
    "--tools",
    PLAN_WORKER_TOOL_LIST,
    "--strict-mcp-config",
];

/// Every [`AgentEffort`] but the fallback: `--effort` takes them all. Claude Code downgrades
/// `xhigh` on models that lack it, and only warns about a level it doesn't know, so plxd sends
/// only these.
const EFFORTS: &[AgentEffort] = &[
    AgentEffort::Low,
    AgentEffort::Medium,
    AgentEffort::High,
    AgentEffort::Xhigh,
    AgentEffort::Max,
];

/// The context windows a run may ask for, in tokens. Claude Code 2.1.286 runs Opus 5.5, Fable
/// 5.1, and Sonnet 5 with 1M by default, which [`DISABLE_1M_ENV`] caps at 200k; Haiku 4.5 has
/// only 200k. So 1M passes nothing.
const CONTEXT_WINDOWS: &[u32] = &[200_000, 1_000_000];

/// Set to `1` for a run that asks for a 200k context window.
const DISABLE_1M_ENV: &str = "CLAUDE_CODE_DISABLE_1M_CONTEXT";

/// Claude Code's permission modes, in the order its own picker lists them (0027).
const PERMISSIONS: &[AgentPermission] = &[
    AgentPermission::Auto,
    AgentPermission::Manual,
    AgentPermission::Edit,
    AgentPermission::Plan,
    AgentPermission::Bypass,
];

/// The oldest Claude Code that has every flag and setting a worker relies on: `--restricted`
/// arrived in 2.1.248, the last of them (0013). An older CLI rejects the unknown flag, and a
/// worker whose `system/init` reports an older version fails, but #156 also checks the detected
/// version before it starts one, for a clearer error. Linux workers need 2.1.275 or later:
/// `linux_sandbox::check_host` reads a `sandbox status` field that arrived then (RYA-112).
pub const WORKER_MIN_VERSION: &str = "2.1.248";

/// Prefixes of inherited variables no run gets: Anthropic credentials, endpoints, profiles, and
/// federation (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`,
/// `ANTHROPIC_PROFILE`, ...), the cloud provider switches (`CLAUDE_CODE_USE_BEDROCK`, ...), and
/// OAuth tokens (`CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`).
pub const SCRUBBED_PREFIXES: &[&str] = &["ANTHROPIC_", "CLAUDE_CODE_USE_", "CLAUDE_CODE_OAUTH_"];

/// Inherited variables no run gets, besides [`SCRUBBED_PREFIXES`]: Bedrock's API key; the
/// configuration folder, which [`apply_credential`] sets only to the account's own;
/// [`SCRUB_ENV`], which a plain no-write run sets itself and a worker or a coordinator must not
/// get (RYA-112, 0027); and [`TASK_LIST_ENV`], which would share one task list (RYA-251).
pub const SCRUBBED_VARS: &[&str] = &[
    "AWS_BEARER_TOKEN_BEDROCK",
    CONFIG_DIR_ENV,
    SCRUB_ENV,
    TASK_LIST_ENV,
];

/// The variable that picks Claude Code's task list (RYA-251). 2.1.283 keeps a session's list in
/// `tasks/<session id>` in the configuration folder, unless this is set and not empty: then every
/// session with the same value shares one list, other threads and the user's own Claude Code
/// sessions included, and a run with the task tools could read, change, or delete their tasks.
/// No run inherits it ([`SCRUBBED_VARS`]), and every run's `--settings` sets it empty
/// ([`settings_env`]).
pub const TASK_LIST_ENV: &str = "CLAUDE_CODE_TASK_LIST_ID";

/// The variable that picks a second account's configuration folder.
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// What `system/init` reports as `apiKeySource` for a subscription login.
pub const SUBSCRIPTION_KEY_SOURCE: &str = "none";

/// The variable an API key account's key is injected as (0004's table, #118).
pub const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// What `system/init` reports as `apiKeySource` for an API key account. Happens to be the same
/// string as [`API_KEY_ENV`] (0004's table), but the two names are checked independently.
pub const API_KEY_SOURCE: &str = "ANTHROPIC_API_KEY";

/// Variables every run gets: report a startup failure as a `result` instead of on stderr alone.
const ALWAYS_SET: &[(&str, &str)] = &[("CLAUDE_CODE_STARTUP_FAILURE_RESULTS", "1")];

/// Set to `1` for a no-write run, to keep credentials out of the CLI's own subprocesses, such as
/// plxd's MCP server (0004 Consequences). A worker doesn't get it: on Linux it swaps in Claude
/// Code's CI sandbox profile, which lets commands write all of `/home`, `/tmp`, `/var`, `/opt`,
/// `/run`, `/mnt`, and `/root` (RYA-20). [`worker_settings`] withholds [`WORKER_WITHHELD_VARS`]
/// from a worker's commands instead, and [`SCRUBBED_VARS`] keeps an inherited one out. Managed
/// settings can still set it, and their `env` beats plxd's, so on Linux
/// `linux_sandbox::check_host` refuses a worker when Claude Code runs with it on (RYA-112), and
/// on every OS a worker whose `system/init` shows the permission mode it forces fails
/// (RYA-118). A coordinator doesn't get it either: it would force `default`, where headless Claude
/// Code denies every tool nobody approved, so a coordinator that reports another mode than it
/// asked for fails the same way (0027). It forces a plain no-write run's mode to `default` as
/// well, so those aren't checked.
const SCRUB_ENV: &str = "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB";

/// Variables a worker's commands never see (0013): an API key account's key, and the token for
/// Claude Code's own messaging socket. `sandbox.credentials` unsets them for each sandboxed
/// command, as [`SCRUB_ENV`] would.
pub const WORKER_WITHHELD_VARS: &[&str] = &[API_KEY_ENV, "CLAUDE_CODE_MESSAGING_TOKEN"];

/// Paths Claude Code 2.1.283's sandbox lets every command write whatever the settings say, which
/// a worker's `denyWrite` takes back (RYA-130): `/tmp/claude` in both spellings, npm's log folder,
/// and Claude Code's debug logs. `~/` is the CLI's `HOME`. A `denyWrite` rule beats them.
pub const WORKER_DENIED_WRITES: &[&str] = &[
    "/tmp/claude",
    "/private/tmp/claude",
    "~/.npm/_logs",
    "~/.claude/debug",
];

/// The variable naming Claude Code's temp folder, which it uses `claude-<uid>` in, apart from the
/// process's own `TMPDIR`.
pub const TEMP_ENV: &str = "CLAUDE_CODE_TMPDIR";

/// The longest `$CLAUDE_CODE_TMPDIR/claude-<uid>` that Claude Code 2.1.283 gives a command as
/// `TMPDIR`. A longer one gets the shared `/tmp/claude-<uid>` instead, which a worker can't use.
pub const MAX_COMMAND_TEMP_BYTES: usize = 44;

/// The folder a worker's CLI gets as [`TEMP_ENV`]: `temp`, the run's own (RYA-130), spelled as
/// short as it can be. On macOS, `/private/tmp/...` becomes `/tmp/...`, where `/tmp`
/// links, which saves 8 of the [`MAX_COMMAND_TEMP_BYTES`].
///
/// # Errors
///
/// [`StartError::Invalid`] if [`commands_temp`] in it is longer than [`MAX_COMMAND_TEMP_BYTES`].
pub fn worker_temp(temp: &Path) -> Result<PathBuf, StartError> {
    let temp = match temp.strip_prefix("/private/tmp") {
        Ok(rest) if cfg!(target_os = "macos") => Path::new("/tmp").join(rest),
        _ => temp.to_owned(),
    };
    let commands = commands_temp(&temp);
    if commands.as_os_str().len() > MAX_COMMAND_TEMP_BYTES {
        return Err(StartError::Invalid(format!(
            "the worker's temp folder {} is longer than {MAX_COMMAND_TEMP_BYTES} bytes, so Claude \
             Code would give its commands the one every session shares instead",
            commands.display()
        )));
    }
    Ok(temp)
}

/// `<temp>/claude-<uid>`: the folder Claude Code makes in its temp folder `temp` and gives
/// sandboxed commands as their `TMPDIR`, which its sandbox lets them write.
#[must_use]
pub fn commands_temp(temp: &Path) -> PathBuf {
    #[cfg(unix)]
    let uid = rustix::process::getuid().as_raw();
    // What Claude Code uses where there is no uid.
    #[cfg(not(unix))]
    let uid = 0;
    temp.join(format!("claude-{uid}"))
}

/// The variable naming a script that Claude Code reads and runs before each Bash command, after
/// the shell's startup files and its own shell snapshot (2.1.283). See [`write_env_file`].
pub const ENV_FILE_ENV: &str = "CLAUDE_ENV_FILE";

/// Writes a worker's [`ENV_FILE_ENV`] script into `dir`, which no worker may read or write
/// (plxd's data folder's `tmp/`), and returns its path, which deletes the file when dropped. The
/// script puts `path`, the `PATH` the CLI started with, in front of whatever `PATH` the shell's
/// startup files left (RYA-126). The file is new, has a random name, and only its owner may read
/// or write it.
///
/// # Errors
///
/// If `dir` can't be created or the file can't be written.
pub fn write_env_file(dir: &Path, path: &OsStr) -> io::Result<TempPath> {
    std::fs::create_dir_all(dir)?;
    let mut file = tempfile::Builder::new()
        .prefix("claude-env-")
        .suffix(".sh")
        .tempfile_in(dir)?;
    file.write_all(&prepend_path_line(path))?;
    Ok(file.into_temp_path())
}

/// A [`Backend`] that runs Claude Code.
#[derive(Clone, Debug)]
pub struct ClaudeBackend {
    launcher: Launcher,
    program: OsString,
    cancel: CancelPolicy,
    limits: OutputLimits,
}

impl ClaudeBackend {
    /// A backend that starts `claude` through `launcher`.
    #[must_use]
    pub fn new(launcher: Launcher) -> Self {
        Self {
            launcher,
            program: PROGRAM.into(),
            cancel: CancelPolicy::default(),
            limits: OutputLimits::default(),
        }
    }

    /// Runs `program`, a name on `PATH` or an absolute path, instead of `claude`.
    #[must_use]
    pub fn with_program(mut self, program: impl Into<OsString>) -> Self {
        self.program = program.into();
        self
    }

    /// Cancels with `policy` instead of `SIGINT` and a 10 s grace period.
    #[must_use]
    pub fn with_cancel_policy(mut self, policy: CancelPolicy) -> Self {
        self.cancel = policy;
        self
    }

    /// Reads output with `limits` instead of the defaults.
    #[must_use]
    pub fn with_limits(mut self, limits: OutputLimits) -> Self {
        self.limits = limits;
        self
    }

    /// What every run and the command list start from: the program in `cwd`, inherited
    /// credentials [`scrubbed`] and `credential`'s injected ([`apply_credential`], whose
    /// `apiKeySource` it returns), [`ALWAYS_SET`], stdin piped, and the backend's output limits.
    fn spec(
        &self,
        cwd: &Path,
        credential: &Credential,
    ) -> Result<(ProcessSpec, &'static str), StartError> {
        let mut spec = ProcessSpec::new(self.program.clone(), cwd);
        spec.scrub = scrubbed(self.launcher.base());
        let key_source = apply_credential(credential, &mut spec)?;
        for (name, value) in ALWAYS_SET {
            spec.inject.set(name, value);
        }
        spec.stdin = StdinMode::Piped;
        spec.limits = self.limits;
        Ok((spec, key_source))
    }
}

/// The CLI's arguments for `request`.
///
/// # Errors
///
/// [`StartError::Invalid`] if the model or the resume id could be read as an option, if a
/// worker has no usable [`WorkerSandbox`], or if a no-write run other than a coordinator asks for
/// a permission.
/// [`StartError::Unsupported`] for an effort or permission this version doesn't know.
pub fn arguments(request: &RunRequest) -> Result<Vec<OsString>, StartError> {
    let coordinator = request.coordinator_tools.is_some();
    if coordinator && request.policy != ToolPolicy::NoWrite {
        return Err(StartError::Invalid(
            "plxd's coordinator tools are only for a no-write run".into(),
        ));
    }
    let unsandboxed = unsandboxed(request);
    let policy: &[&str] = match request.policy {
        ToolPolicy::NoWrite if coordinator => &[],
        ToolPolicy::NoWrite => NO_WRITE_ARGS,
        ToolPolicy::WorkspaceWrite if unsandboxed => &[],
        ToolPolicy::WorkspaceWrite if hands_over_plans(request) => PLAN_WORKSPACE_WRITE_ARGS,
        ToolPolicy::WorkspaceWrite => WORKSPACE_WRITE_ARGS,
    };
    let mut args: Vec<OsString> = BASE_ARGS.iter().chain(policy).map(Into::into).collect();
    // Headless Claude Code turns fast mode on only when the flag settings opt in.
    let settings = |mut settings: Value| -> OsString {
        if let Some(fast) = request.fast {
            settings["fastMode"] = fast.into();
        }
        settings.to_string().into()
    };
    if request.policy == ToolPolicy::NoWrite && !coordinator {
        if request.permission.is_some() {
            return Err(StartError::Invalid(
                "a no-write run takes no permission; its mode is fixed (0004)".into(),
            ));
        }
        args.extend(["--settings".into(), settings(no_write_settings())]);
    } else {
        let mode = permission_mode(request.permission)?;
        args.extend(["--permission-mode".into(), mode.into()]);
        if prompts(request) {
            args.extend(PROMPT_TOOL_ARGS.iter().map(Into::into));
        }
    }
    if let Some(tools) = &request.coordinator_tools {
        let allowed: Vec<&str> = mcp::ALLOWED_TOOLS
            .iter()
            .chain(TODO_TOOLS)
            .copied()
            .collect();
        args.extend([
            "--mcp-config".into(),
            tools.mcp_config()?.to_string().into(),
            "--allowedTools".into(),
            allowed.join(",").into(),
        ]);
    } else if unsandboxed {
        args.extend(["--allowedTools".into(), TODO_TOOLS.join(",").into()]);
    }
    if let Some(sandbox) = worker_sandbox(request)? {
        if !unsandboxed {
            let config_home = match &request.account.credential {
                Credential::Subscription { config_home } => config_home.as_deref(),
                Credential::ApiKey(_) => None,
            };
            let worker = worker_settings(sandbox, &request.cwd, config_home);
            args.extend(["--settings".into(), settings(worker)]);
        }
        for dir in &sandbox.writable {
            args.extend(["--add-dir".into(), dir.into()]);
        }
    }
    if coordinator || unsandboxed {
        // Full Claude Code loads the user's and the project's settings, whose `env` could share
        // its task list, so it gets `--settings` only for this (RYA-251).
        let env = serde_json::json!({"env": settings_env()});
        args.extend(["--settings".into(), settings(env)]);
    }
    if let Some(model) = &request.model {
        check_argument("model", model)?;
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(effort) = request.effort {
        args.extend(["--effort".into(), effort_level(effort)?.into()]);
    }
    if let Some(resume) = &request.resume {
        check_argument("resume id", &resume.session_id)?;
        args.extend(["--resume".into(), resume.session_id.clone().into()]);
        if resume.fork {
            args.push("--fork-session".into());
        }
    }
    Ok(args)
}

/// The `--settings` a worker runs with (0013): hooks off; Bash and the web tools allowed; and
/// Claude Code's Bash sandbox on, with no way around it, `sandbox`'s paths, every host but
/// [`WORKER_DENIED_HOSTS`], and no [`WORKER_WITHHELD_VARS`]. `WebFetch(domain:*)` is what opens
/// the network: the sandbox takes its allowlist from `WebFetch` allow rules, and a bare `*`
/// matches every host. The denied hosts are `WebFetch` deny rules as well as `deniedDomains`,
/// because the sandbox's list binds only commands, and a deny rule beats the `*` allow for the
/// tool. Bash is an allow rule as well, not only `autoAllowBashIfSandboxed`, so it stays allowed
/// if managed settings force permission mode `default` (RYA-112). `cwd`, the writable folders,
/// the read-only git paths, and the commands' `TMPDIR` in the run's temp folder
/// ([`commands_temp`], which Claude Code lets them write) stay readable inside an unreadable
/// path, such as plxd's data folder, which holds the worktree, the context folder, and a normal
/// thread's scratch repository (#110). The rest of the temp folder stays hidden: the CLI's own
/// unsandboxed processes keep files there. A second account's `config_home` is unreadable too.
/// [`WORKER_DENIED_WRITES`] aren't writable. Like every run's, it holds [`settings_env`].
#[must_use]
pub fn worker_settings(sandbox: &WorkerSandbox, cwd: &Path, config_home: Option<&Path>) -> Value {
    let unreadable = strings(
        sandbox
            .unreadable
            .iter()
            .map(PathBuf::as_path)
            .chain(config_home),
    );
    let readable = strings(
        std::iter::once(cwd)
            .chain(sandbox.writable.iter().map(PathBuf::as_path))
            .chain(sandbox.read_only.iter().map(PathBuf::as_path))
            .chain([commands_temp(&sandbox.temp).as_path()]),
    );
    let mut read_only = strings(sandbox.read_only.iter().map(PathBuf::as_path));
    read_only.extend(WORKER_DENIED_WRITES.iter().map(|&path| path.to_owned()));
    let denied_fetches: Vec<String> = WORKER_DENIED_HOSTS
        .iter()
        .map(|host| format!("WebFetch(domain:{host})"))
        .collect();
    let withheld: Vec<Value> = WORKER_WITHHELD_VARS
        .iter()
        .map(|name| serde_json::json!({"name": name, "mode": "deny"}))
        .collect();
    serde_json::json!({
        "disableAllHooks": true,
        "permissions": {
            "allow": ["Bash", "WebFetch(domain:*)", "WebSearch"],
            "deny": denied_fetches,
        },
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "autoAllowBashIfSandboxed": true,
            "allowUnsandboxedCommands": false,
            "excludedCommands": [],
            "network": {
                "strictAllowlist": true,
                "deniedDomains": WORKER_DENIED_HOSTS,
                "allowLocalBinding": false,
            },
            "filesystem": {
                "denyRead": unreadable,
                "allowRead": readable,
                "denyWrite": read_only,
            },
            "credentials": {"envVars": withheld},
        },
        "env": settings_env(),
    })
}

/// Paths as settings strings. [`worker_sandbox`] has already refused any that isn't UTF-8.
fn strings<'a>(paths: impl Iterator<Item = &'a Path>) -> Vec<String> {
    paths
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

/// Whether `request` runs as full Claude Code with no worker sandbox, though it may write: a
/// normal thread in any mode whose client answers permission requests ([`full_thread`], 0034),
/// or a worker in [`AgentPermission::Bypass`] (0027). Its tools, settings, and MCP servers are
/// whatever the user's configuration loads.
#[must_use]
pub fn unsandboxed(request: &RunRequest) -> bool {
    request.policy == ToolPolicy::WorkspaceWrite
        && (full_thread(request) || request.permission == Some(AgentPermission::Bypass))
}

/// Whether `request` is a normal thread that runs as full Claude Code (0034): one whose client
/// answers permission requests. Without `approvals`, such as a thread started before the app
/// showed them, its commands would be denied wherever they'd prompt, so it keeps the worker
/// sandbox, where they run without asking, as before.
#[must_use]
pub fn full_thread(request: &RunRequest) -> bool {
    request.thread && request.approvals
}

/// Whether `request`'s CLI asks plxd before a tool call that would prompt (RYA-222, 0031): a
/// worker, a thread, or a coordinator in Manual, Auto, or Plan, and a thread in Accept Edits too,
/// whose client answers ([`RunRequest::approvals`]). A thread has no sandbox to run its commands
/// without asking, so in Accept Edits its Bash calls prompt, as in a terminal (0034). A sandboxed
/// worker's don't, and Bypass Permissions asks about nothing. A plain no-write run denies
/// anything not allowed (`dontAsk`), and a run without `approvals` denies what would prompt, as
/// headless Claude Code does.
#[must_use]
pub fn prompts(request: &RunRequest) -> bool {
    let plain_no_write =
        request.policy == ToolPolicy::NoWrite && request.coordinator_tools.is_none();
    let thread_edits =
        full_thread(request) && matches!(request.permission, None | Some(AgentPermission::Edit));
    request.approvals
        && !plain_no_write
        && (thread_edits
            || matches!(
                request.permission,
                Some(AgentPermission::Manual | AgentPermission::Auto | AgentPermission::Plan)
            ))
}

/// Whether `request`'s worker also gets `ExitPlanMode` ([`PLAN_WORKSPACE_WRITE_ARGS`], RYA-243):
/// a sandboxed worker in Plan whose CLI asks plxd ([`prompts`]). A thread has no `--tools`, so
/// it has the tool as a coordinator does (0034). Headless Claude Code offers
/// the tool only to a run that asks a host, and asking with it is how the plan reaches the user
/// (0031). The tool runs nothing and writes nothing in the worktree. Allowing it only moves the
/// CLI to `default` (Manual), a mode a worker can start in, under the same `--restricted`,
/// `--tools`, and `--settings`, which no mode change drops. After it, the `system/init` check
/// accepts only `default` and `acceptEdits` besides `plan`. Every other worker keeps
/// [`WORKSPACE_WRITE_ARGS`].
#[must_use]
pub fn hands_over_plans(request: &RunRequest) -> bool {
    request.policy == ToolPolicy::WorkspaceWrite
        && !full_thread(request)
        && request.permission == Some(AgentPermission::Plan)
        && prompts(request)
}

/// A worker's or a coordinator's `--permission-mode` for `permission`: Claude Code's own mode of
/// the same name (RYA-97, 0027), [`DEFAULT_PERMISSION_MODE`] by default.
///
/// # Errors
///
/// [`StartError::Unsupported`] for a permission this version doesn't know.
pub fn permission_mode(permission: Option<AgentPermission>) -> Result<&'static str, StartError> {
    match permission {
        None | Some(AgentPermission::Edit) => Ok(DEFAULT_PERMISSION_MODE),
        Some(AgentPermission::Auto) => Ok("auto"),
        Some(AgentPermission::Manual) => Ok("default"),
        Some(AgentPermission::Plan) => Ok("plan"),
        Some(AgentPermission::Bypass) => Ok(BYPASS_PERMISSION_MODE),
        Some(AgentPermission::Unknown) => Err(StartError::Unsupported(
            "Claude Code has no mode for this permission".into(),
        )),
    }
}

/// `--effort`'s value for `effort`.
fn effort_level(effort: AgentEffort) -> Result<&'static str, StartError> {
    Ok(match effort {
        AgentEffort::Low => "low",
        AgentEffort::Medium => "medium",
        AgentEffort::High => "high",
        AgentEffort::Xhigh => "xhigh",
        AgentEffort::Max => "max",
        AgentEffort::Unknown => {
            return Err(StartError::Unsupported(
                "Claude Code has no such effort level".into(),
            ));
        }
    })
}

/// The variables of `base` that no run gets: [`SCRUBBED_PREFIXES`] and [`SCRUBBED_VARS`].
#[must_use]
pub fn scrubbed(base: &Environment) -> Vec<OsString> {
    base.names()
        .filter(|name| {
            let bytes = name.as_encoded_bytes();
            SCRUBBED_PREFIXES
                .iter()
                .any(|prefix| bytes.starts_with(prefix.as_bytes()))
                || SCRUBBED_VARS.iter().any(|var| var.as_bytes() == bytes)
        })
        .map(OsStr::to_owned)
        .collect()
}

/// Injects what `credential` needs into `spec`, after [`scrubbed`] removed every inherited
/// credential, and returns the `apiKeySource` that `system/init` must then report.
///
/// # Errors
///
/// Never today; kept fallible so a future credential kind this backend can't serve has somewhere
/// to report it, the way [`StartError::Unsupported`] already does elsewhere in this module.
pub fn apply_credential(
    credential: &Credential,
    spec: &mut ProcessSpec,
) -> Result<&'static str, StartError> {
    match credential {
        Credential::Subscription { config_home } => {
            if let Some(home) = config_home {
                spec.inject.set(CONFIG_DIR_ENV, home);
            }
            Ok(SUBSCRIPTION_KEY_SOURCE)
        }
        Credential::ApiKey(key) => {
            spec.inject.set(API_KEY_ENV, key.expose());
            Ok(API_KEY_SOURCE)
        }
    }
}

impl Backend for ClaudeBackend {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            follow_ups: true,
            resume: true,
            coordinator: true,
            reports_cost: true,
            rate_limits: true,
            worker_sandbox: cfg!(any(target_os = "macos", target_os = "linux")),
            fork: true,
        }
    }

    fn efforts(&self) -> &'static [AgentEffort] {
        EFFORTS
    }

    fn permissions(&self) -> &'static [AgentPermission] {
        PERMISSIONS
    }

    fn context_windows(&self) -> &'static [u32] {
        CONTEXT_WINDOWS
    }

    fn fast_mode(&self) -> bool {
        true
    }

    /// `claude -p` in stream-json on the user's login, asked to `initialize`
    /// ([`commands::claude`]).
    fn commands(&self, cwd: &Path) -> Result<Option<CommandsProbe>, StartError> {
        let (mut spec, _) = self.spec(cwd, &Credential::Subscription { config_home: None })?;
        spec.args = BASE_ARGS.iter().map(OsString::from).collect();
        Ok(Some(CommandsProbe {
            process: self.launcher.spawn(&spec)?,
            input: vec![json!({
                "type": "control_request",
                "request_id": "commands",
                "request": {"subtype": "initialize"},
            })],
            parse: commands::claude,
        }))
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        // Images alone are a message too (RYA-202), as a resumed run's may be.
        if request.prompt.is_empty() && request.images.is_empty() {
            return Err(StartError::Invalid("the prompt is empty".into()));
        }
        if request.policy != ToolPolicy::NoWrite && !self.capabilities().worker_sandbox {
            return Err(StartError::Unsupported(
                "plxd can't check Claude Code's worker sandbox on this OS yet (decision 0023)"
                    .into(),
            ));
        }
        let (mut spec, expected_key_source) =
            self.spec(&request.cwd, &request.account.credential)?;
        spec.args = arguments(&request)?;
        let asks = prompts(&request);
        let plan_exit = hands_over_plans(&request);
        let full = full_thread(&request);
        if let Some(sandbox) = worker_sandbox(&request)? {
            spec.inject.set(TEMP_ENV, worker_temp(&sandbox.temp)?);
        }
        if request.policy == ToolPolicy::NoWrite && request.coordinator_tools.is_none() {
            spec.inject.set(SCRUB_ENV, "1");
        }
        match request.context_window {
            None | Some(1_000_000) => {}
            Some(200_000) => {
                spec.inject.set(DISABLE_1M_ENV, "1");
            }
            Some(tokens) => {
                return Err(StartError::Unsupported(format!(
                    "Claude Code has no {tokens}-token context window"
                )));
            }
        }
        let env_file = match self.launcher.base().get("PATH") {
            Some(path) if request.policy == ToolPolicy::WorkspaceWrite => {
                let dir = self.launcher.data_dir().temp_dir();
                let file = write_env_file(&dir, path).map_err(SpawnError::Io)?;
                spec.inject.set(ENV_FILE_ENV, file.as_os_str());
                Some(file)
            }
            _ => None,
        };

        let process = self.launcher.spawn(&spec)?;
        let switch = CancelSwitch::new();
        switch.arm(process.signals().clone(), self.cancel);
        let (handle, control) = RunHandle::new(request.run_id, true, switch.clone());
        let (handle, answers) = handle.with_answers();
        let stop = Arc::new(Notify::new());
        let baseline = request
            .resume
            .map(|resume| resume.usage_totals)
            .unwrap_or_default();
        let (sink, events) = EventSink::channel(EVENT_BUFFER, baseline);
        let driver = Driver {
            process,
            control,
            answers,
            sink,
            switch,
            stop: Arc::clone(&stop),
            translator: Translator::new(request.policy, expected_key_source)
                .with_coordinator_tools(request.coordinator_tools.is_some())
                .with_thread(full)
                .with_permission_mode(permission_mode(request.permission)?)
                .with_prompts(asks)
                .with_plan_exit(plan_exit),
            turns: VecDeque::new(),
            asks: HashMap::new(),
            violation: None,
            env_file,
        };
        let prompt = Message::new(request.turn_id, &request.prompt, &request.images, false);
        tokio::spawn(driver.run(prompt));
        Ok(Started {
            run: Arc::new(ClaudeRun { handle, stop }),
            events,
        })
    }
}

/// The run's handle: [`RunHandle`], plus closing stdin on cancel so the CLI exits after the
/// interrupted turn instead of waiting for more input until the grace period ends.
struct ClaudeRun {
    handle: RunHandle,
    stop: Arc<Notify>,
}

impl Run for ClaudeRun {
    fn id(&self) -> RunId {
        self.handle.id()
    }

    fn send(&self, message: FollowUp) -> Result<(), SendError> {
        self.handle.send(message)
    }

    fn cancel(&self) {
        self.handle.cancel();
        self.stop.notify_one();
    }

    fn answer(&self, answer: Answer) -> Result<(), AnswerError> {
        self.handle.answer(answer)
    }
}

/// A user message for the CLI's stdin.
#[derive(Debug)]
struct Message {
    turn_id: Option<TurnId>,
    uuid: String,
    line: String,
    follow_up: bool,
}

impl Message {
    /// The message's content is `text` alone, or with images, the Messages API's base64 image
    /// blocks and then `text` as a text block (RYA-191). A message of images alone has no text
    /// block, since the API refuses a blank one (RYA-193).
    fn new(turn_id: Option<TurnId>, text: &str, images: &[PromptImage], follow_up: bool) -> Self {
        let uuid = turn_id.unwrap_or_else(TurnId::generate).to_string();
        let content = if images.is_empty() {
            Value::from(text)
        } else {
            let images = images.iter().map(|image| {
                serde_json::json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": image.media_type, "data": image.data},
                })
            });
            let text = (!text.trim().is_empty())
                .then(|| serde_json::json!({"type": "text", "text": text}));
            images.chain(text).collect()
        };
        let mut line = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": content},
            "parent_tool_use_id": null,
            "uuid": uuid,
        })
        .to_string();
        line.push('\n');
        Self {
            turn_id,
            uuid,
            line,
            follow_up,
        }
    }

    /// A `control_response` to one of the CLI's control requests (RYA-222).
    fn control(response: &Value) -> Self {
        let mut line =
            serde_json::json!({"type": "control_response", "response": response}).to_string();
        line.push('\n');
        Self {
            turn_id: None,
            uuid: String::new(),
            line,
            follow_up: false,
        }
    }
}

/// The `control_response` payload that answers `ask` with `decision`, as the Agent SDK writes it.
/// An allow always carries `updatedInput`, which older CLIs require.
fn answer_response(ask: &Ask, decision: Decision) -> Value {
    let mut response = match decision {
        Decision::Allow { input, always } => {
            let input = input.unwrap_or_else(|| ask.input.clone());
            let mut allow = serde_json::json!({"behavior": "allow", "updatedInput": input});
            if always && !ask.updates.is_empty() {
                allow["updatedPermissions"] = Value::from(ask.updates.clone());
            }
            allow
        }
        Decision::Deny { message, interrupt } => {
            serde_json::json!({"behavior": "deny", "message": message, "interrupt": interrupt})
        }
    };
    if let Some(id) = &ask.tool_use_id {
        response["toolUseID"] = Value::from(id.as_str());
    }
    serde_json::json!({"subtype": "success", "request_id": ask.request_id, "response": response})
}

/// The result of writing one message to stdin.
enum Delivery {
    Written(Message),
    Failed(Message),
}

/// Writes messages to stdin in order, off the driver's loop, so a CLI that stops reading stdin
/// can't keep the driver from reading its stdout.
async fn write_messages(
    mut stdin: StdinPipe,
    mut queue: mpsc::UnboundedReceiver<Message>,
    results: mpsc::UnboundedSender<Delivery>,
) {
    let mut broken = false;
    while let Some(message) = queue.recv().await {
        broken = broken || stdin.write_all(message.line.as_bytes()).await.is_err();
        let result = if broken {
            Delivery::Failed(message)
        } else {
            Delivery::Written(message)
        };
        let _ = results.send(result);
    }
}

/// The CLI's stdin: a queue into [`write_messages`], and its results.
struct Stdin {
    queue: Option<mpsc::UnboundedSender<Message>>,
    results: mpsc::UnboundedReceiver<Delivery>,
    writer: Option<tokio::task::JoinHandle<()>>,
    /// Messages queued whose delivery hasn't been read yet.
    pending: usize,
}

impl Stdin {
    fn start(process: &mut Process) -> Self {
        let (results_tx, results) = mpsc::unbounded_channel();
        let Some(pipe) = process.take_stdin() else {
            return Self {
                queue: None,
                results,
                writer: None,
                pending: 0,
            };
        };
        let (queue, queue_rx) = mpsc::unbounded_channel();
        Self {
            queue: Some(queue),
            results,
            writer: Some(tokio::spawn(write_messages(pipe, queue_rx, results_tx))),
            pending: 0,
        }
    }

    fn is_open(&self) -> bool {
        self.queue.is_some()
    }

    /// Queues `message`, or gives it back once stdin is closed.
    fn send(&mut self, message: Message) -> Result<(), Message> {
        let Some(queue) = &self.queue else {
            return Err(message);
        };
        queue.send(message).map_err(|error| error.0)?;
        self.pending += 1;
        Ok(())
    }

    /// Closes stdin once the writer has written what is queued.
    fn close(&mut self) {
        self.queue = None;
    }
}

/// One run: forwards the CLI's events, delivers follow-ups, and decides the outcome when the CLI
/// exits. Cancelling doesn't wait for it: the handle signals the process through the switch.
struct Driver {
    process: Process,
    control: mpsc::UnboundedReceiver<FollowUp>,
    answers: mpsc::UnboundedReceiver<Answer>,
    sink: EventSink,
    switch: CancelSwitch,
    stop: Arc<Notify>,
    translator: Translator,
    /// Turns the CLI has been sent but hasn't finished, oldest first: their ids and `uuid`s.
    turns: VecDeque<(Option<TurnId>, String)>,
    /// Permission requests the CLI waits on (RYA-222).
    asks: HashMap<ApprovalId, Ask>,
    violation: Option<Failure>,
    /// A worker's [`ENV_FILE_ENV`] script, deleted once the CLI has exited.
    env_file: Option<TempPath>,
}

impl Driver {
    async fn run(mut self, prompt: Message) {
        let mut stdin = Stdin::start(&mut self.process);
        self.turns.push_back((prompt.turn_id, prompt.uuid.clone()));
        let first = Event::TurnStarted {
            turn_id: prompt.turn_id,
        };
        if self.sink.emit(first).await.is_err() {
            self.switch.cancel();
        }
        if stdin.send(prompt).is_err() {
            self.control.close();
        }
        let mut control_open = true;
        let mut answers_open = true;

        let exit = loop {
            tokio::select! {
                // Deliveries first, so a follow-up's TurnStarted comes before what the CLI answers.
                biased;
                Some(delivery) = stdin.results.recv() => {
                    stdin.pending = stdin.pending.saturating_sub(1);
                    match delivery {
                        Delivery::Written(message) if message.follow_up => {
                            self.turns.push_back((message.turn_id, message.uuid));
                            let started = Event::TurnStarted { turn_id: message.turn_id };
                            self.emit(started).await;
                        }
                        Delivery::Written(_) => {}
                        Delivery::Failed(message) => {
                            // stdin is gone, so no later message can arrive either.
                            stdin.close();
                            self.control.close();
                            self.dropped(&message).await;
                        }
                    }
                }
                output = self.process.next() => match output {
                    Some(Output::Line(line)) => {
                        if self.violation.is_none() {
                            let steps = self.translator.line(&line);
                            self.apply(steps, &mut stdin).await;
                        }
                    }
                    Some(Output::Oversized { bytes }) => {
                        self.emit(Event::Warning {
                            warning: WarningKind::OversizedLine,
                            detail: format!("skipped a {bytes}-byte line"),
                        })
                        .await;
                    }
                    Some(Output::Exited(exit)) => break Some(exit),
                    None => break None,
                },
                // Answers before follow-ups: the CLI is waiting on them.
                answer = self.answers.recv(), if answers_open => match answer {
                    Some(answer) => self.answer(answer, &mut stdin),
                    None => answers_open = false,
                },
                follow_up = self.control.recv(), if control_open => match follow_up {
                    Some(follow_up) => {
                        let message = Message::new(
                            Some(follow_up.turn_id),
                            &follow_up.text,
                            &follow_up.images,
                            true,
                        );
                        if let Err(message) = stdin.send(message) {
                            self.dropped(&message).await;
                        }
                    }
                    None => control_open = false,
                },
                () = self.stop.notified(), if stdin.is_open() => {
                    stdin.close();
                    self.control.close();
                }
                () = self.sink.closed(), if !self.switch.is_cancelled() => {
                    self.switch.cancel();
                    stdin.close();
                    self.control.close();
                }
            }
            self.close_when_idle(&mut stdin);
        };

        self.drop_undelivered(stdin).await;
        self.withdraw_left().await;
        self.env_file = None;
        let outcome = self.outcome(exit);
        let _ = self.sink.finish(outcome).await;
    }

    /// Withdraws every request the CLI still waited on when it exited, so each one ends in this
    /// attempt's own events, whatever an account fallback (#119) does with its `Finished`.
    async fn withdraw_left(&mut self) {
        for approval_id in std::mem::take(&mut self.asks).into_keys() {
            self.emit(Event::ApprovalWithdrawn { approval_id }).await;
        }
    }

    async fn apply(&mut self, steps: Vec<Step>, stdin: &mut Stdin) {
        for step in steps {
            match step {
                Step::Emit(event) => self.emit(event).await,
                Step::Total(total) => {
                    let observed = self
                        .sink
                        .observe_total(total.model.as_deref(), total.usage)
                        .await;
                    if observed.is_err() {
                        self.switch.cancel();
                    }
                }
                Step::TurnDone(done) => {
                    for turn_id in self.finish_turns(&done) {
                        let result = done.result.clone();
                        self.emit(Event::TurnFinished { turn_id, result }).await;
                    }
                }
                Step::Violation(failure) => {
                    // Kill at once, not SIGINT: every moment it runs may bill the wrong account.
                    let _ = self.process.signals().signal_group(Signal::KILL);
                    self.violation = Some(failure);
                    stdin.close();
                    self.control.close();
                    return;
                }
                Step::Ask(request, ask) => {
                    self.asks.insert(request.approval_id, ask);
                    self.emit(Event::ApprovalRequested(request)).await;
                }
                Step::Withdraw(request_id) => {
                    let withdrawn = self
                        .asks
                        .iter()
                        .find(|(_, ask)| ask.request_id == request_id)
                        .map(|(&approval_id, _)| approval_id);
                    if let Some(approval_id) = withdrawn {
                        self.asks.remove(&approval_id);
                        self.emit(Event::ApprovalWithdrawn { approval_id }).await;
                    }
                }
                Step::Refuse { request_id, error } => {
                    let response = serde_json::json!({
                        "subtype": "error",
                        "request_id": request_id,
                        "error": error,
                    });
                    let _ = stdin.send(Message::control(&response));
                }
            }
        }
    }

    /// Writes `answer` to the CLI, if it still waits on the request.
    fn answer(&mut self, answer: Answer, stdin: &mut Stdin) {
        let Some(ask) = self.asks.remove(&answer.approval_id) else {
            return;
        };
        if ask.tool_name == EXIT_PLAN_MODE && matches!(answer.decision, Decision::Allow { .. }) {
            self.translator.left_plan_mode();
        }
        let response = answer_response(&ask, answer.decision);
        let _ = stdin.send(Message::control(&response));
    }

    /// The turns a `result` ended, oldest first. Turns finish in the order they started, so a
    /// result ends every outstanding turn up to the newest one it names.
    fn finish_turns(&mut self, done: &TurnDone) -> Vec<Option<TurnId>> {
        let named = self
            .turns
            .iter()
            .rposition(|(_, uuid)| done.uuids.contains(uuid));
        let count = match (named, done.queued) {
            (Some(index), _) => index + 1,
            // With no ids to go by and nothing queued, or no count either, every turn sent so far
            // is taken as done. At worst a folded follow-up finishes early; ending only one turn
            // could leave stdin open and the run waiting forever.
            (None, Some(0) | None) if done.uuids.is_empty() => self.turns.len(),
            (None, _) => 1,
        };
        let count = count.min(self.turns.len());
        self.turns
            .drain(..count)
            .map(|(turn_id, _)| turn_id)
            .collect()
    }

    /// Closes stdin once no turn is outstanding and no permission request waits, so the CLI exits
    /// after its last result. Claude Code fails a request whose stdin has closed.
    fn close_when_idle(&mut self, stdin: &mut Stdin) {
        if stdin.is_open() && self.turns.is_empty() && self.asks.is_empty() && stdin.pending == 0 {
            stdin.close();
            self.control.close();
        }
    }

    async fn emit(&mut self, event: Event) {
        if self.sink.emit(event).await.is_err() {
            self.switch.cancel();
        }
    }

    async fn dropped(&mut self, message: &Message) {
        if message.follow_up
            && let Some(turn_id) = message.turn_id
        {
            self.emit(Event::FollowUpDropped { turn_id }).await;
        }
    }

    /// After the CLI exited: reports every follow-up that was sent or queued but never started a
    /// turn as dropped.
    async fn drop_undelivered(&mut self, mut stdin: Stdin) {
        self.control.close();
        let mut late = Vec::new();
        while let Ok(follow_up) = self.control.try_recv() {
            late.push(follow_up);
        }
        for follow_up in late {
            let message = Message::new(
                Some(follow_up.turn_id),
                &follow_up.text,
                &follow_up.images,
                true,
            );
            if let Err(message) = stdin.send(message) {
                self.dropped(&message).await;
            }
        }
        stdin.close();
        if let Some(writer) = stdin.writer.take() {
            // Writes fail at once once nothing holds the pipe's read end. Something the CLI
            // started outside its process group could, so don't wait on it for long.
            let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
        }
        while let Ok(delivery) = stdin.results.try_recv() {
            let (Delivery::Written(message) | Delivery::Failed(message)) = delivery;
            self.dropped(&message).await;
        }
    }

    fn outcome(&mut self, exit: Option<Exit>) -> Outcome {
        if let Some(violation) = self.violation.take() {
            return failed(violation, exit.as_ref());
        }
        if self.switch.is_cancelled() {
            return Outcome::Cancelled;
        }
        if let Some(failure) = self.translator.last_failure.take() {
            return failed(failure, exit.as_ref());
        }
        let Some(exit) = exit else {
            return failed(
                failure(FailureKind::Internal, "lost track of the process".into()),
                None,
            );
        };
        if exit.info.success() && self.translator.results > 0 {
            return Outcome::Completed {
                result: self.translator.last_result.take(),
            };
        }
        let lower = exit.stderr_tail.to_ascii_lowercase();
        let failure = if lower.contains("not logged in") || lower.contains("/login") {
            failure(
                FailureKind::NotSignedIn,
                "Claude Code is not signed in".into(),
            )
        } else if exit.info.success() {
            failure(
                FailureKind::VendorError,
                "Claude Code exited without finishing its turn".into(),
            )
        } else {
            let message = match (exit.info.code, exit.info.signal) {
                (_, Some(signal)) => format!("Claude Code was killed by signal {signal}"),
                (Some(code), None) => format!("Claude Code exited with code {code}"),
                (None, None) => "Claude Code ended in an unknown way".to_owned(),
            };
            failure(FailureKind::Crashed, message)
        };
        failed(failure, Some(&exit))
    }
}

fn failure(failure: FailureKind, message: String) -> Failure {
    Failure {
        failure,
        message,
        exit: None,
        stderr_tail: None,
    }
}

/// A failed outcome, with how the process ended when it did.
fn failed(mut failure: Failure, exit: Option<&Exit>) -> Outcome {
    if let Some(exit) = exit {
        failure.exit = Some(exit.info);
        failure.stderr_tail = (!exit.stderr_tail.is_empty()).then(|| exit.stderr_tail.clone());
    }
    Outcome::Failed(failure)
}
