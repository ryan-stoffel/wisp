//! The Claude Code backend: runs the user's own signed-in `claude` CLI headless (0004, #116).
//!
//! # The command
//!
//! Every run is `claude -p --output-format stream-json --verbose --input-format stream-json` in
//! the run's cwd, plus the policy's flags, `--model`, and `--resume <session id>` (0004 [10]):
//!
//! - **No-write** is exactly 0004's: [`NO_WRITE_ARGS`]. As a second check, a no-write run whose
//!   `system/init` lists any tool outside [`NO_WRITE_TOOLS`] fails with
//!   [`FailureKind::PolicyViolation`].
//! - **Workspace-write** is 0013's worker sandbox: [`WORKSPACE_WRITE_ARGS`], then
//!   [`worker_settings`] as `--settings`, then `--add-dir` for each writable folder:
//!   - `--restricted` loads no user, project, or local settings files, so a repository's
//!     `.claude/settings.json` can't add allow rules, hooks, or an `env` block (#134), and it
//!     confines the file tools to the working directories.
//!   - `--tools` names exactly [`WORKER_TOOLS`]. `Bash` is among them because Claude Code's own
//!     Seatbelt sandbox holds every command: writes only to the working directories and the
//!     session temp folder, no reads of the sandbox's `unreadable` paths, and no writes to git
//!     metadata. `failIfUnavailable` and `allowUnsandboxedCommands: false` keep a command from
//!     ever running outside it. Commands, `WebFetch`, and `WebSearch` reach any host but
//!     [`WORKER_DENIED_HOSTS`] (Ryan, #137), so the unreadable paths are what keep secrets in.
//!   - `--strict-mcp-config` connects no MCP servers, including the repository's `.mcp.json`.
//!
//!   As a second check, a worker whose `system/init` lists a tool outside [`WORKER_TOOLS`], or
//!   a Claude Code older than [`WORKER_MIN_VERSION`], fails with
//!   [`FailureKind::PolicyViolation`].
//!
//! # Messages go on stdin
//!
//! With `--input-format stream-json`, the prompt and every follow-up are user messages on stdin,
//! one JSON object per line, as the Agent SDK sends them. The prompt never goes in argv, where
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
//! the Keychain. The key is never in `args`, so `ps` can't show it, and every copy of it wispd
//! makes along the way ([`super::ApiKey`]'s own buffer, [`super::process::Environment`]'s
//! entries, and the buffers `spawn_session` builds from them) is zeroized once it is done with
//! it.
//!
//! A project's `env` block can still set variables for a worker (0004, #134), so the output is
//! checked as well. A `system/init` whose `apiKeySource` isn't the account's, or is missing, and
//! a `result` whose `modelUsage` names a provider other than `firstParty`, kill the CLI's process
//! group at once and fail the run with [`FailureKind::UnexpectedApiKey`].
//!
//! # Cancel
//!
//! `SIGINT` ends Claude's turn, while `SIGTERM` leaves it unfinished (0004 [11]), so cancel sends
//! `SIGINT`, closes stdin so the CLI exits after the interrupted turn, and kills the process
//! group if it is still running after the grace period.

mod stream;
#[cfg(test)]
mod tests;

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustix::process::Signal;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::net::unix::pipe;
use tokio::sync::{Notify, mpsc};

pub(crate) use self::stream::version as parse_version;
use self::stream::{Step, Translator, TurnDone};
use super::event::{Event, Failure, FailureKind, Outcome, WarningKind};
use super::process::{
    CancelPolicy, Environment, Exit, Launcher, Output, OutputLimits, Process, ProcessSpec,
    StdinMode,
};
use super::sandbox::worker_sandbox;
use super::{
    Backend, CancelSwitch, Capabilities, Credential, EVENT_BUFFER, EventSink, FollowUp, Run,
    RunHandle, RunId, RunRequest, SendError, StartError, Started, ToolPolicy, TurnId,
    WorkerSandbox,
};

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

/// [`ToolPolicy::NoWrite`]'s arguments, exactly as 0004 has them: read-only built-in tools, only
/// the user's settings (so no project `env` block or hooks), hooks off, no MCP servers but
/// wispd's, and every call that would prompt denied.
pub const NO_WRITE_ARGS: &[&str] = &[
    "--tools",
    "Read,Glob,Grep",
    "--setting-sources",
    "user",
    "--settings",
    r#"{"disableAllHooks":true}"#,
    "--strict-mcp-config",
    "--permission-mode",
    "dontAsk",
];

/// The only tools a no-write run's `system/init` may list. `EndConversation` stays whatever
/// `--tools` says (the CLI reference), and only ends the session. wispd's own MCP tools join
/// this list in M4.
pub const NO_WRITE_TOOLS: &[&str] = &["Read", "Glob", "Grep", "EndConversation"];

/// The built-in tools a worker gets (0013): the file tools, `Bash`, which Claude Code's sandbox
/// confines, the web tools (Ryan, #137), and `TodoWrite`. No subagents, skills, or MCP tools.
/// `EndConversation` may appear in `system/init` as well, as for a no-write run.
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
];

/// [`WORKER_TOOLS`] as `--tools` takes them.
pub const WORKER_TOOL_LIST: &str =
    "Read,Edit,Write,Glob,Grep,NotebookEdit,Bash,WebFetch,WebSearch,TodoWrite";

/// The names for this Mac that no worker command or `WebFetch` may reach, even with network
/// access: this Mac's own services wait on #168. The sandbox's proxy canonicalizes other
/// spellings of loopback (`127.1`, `[::ffff:127.0.0.1]`) and refuses names that resolve to this
/// Mac, but it doesn't check IP literals, so the unspecified addresses are listed too. This Mac's
/// interface addresses aren't: 0013 records that gap.
pub const WORKER_DENIED_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]", "0.0.0.0", "[::]"];

/// [`ToolPolicy::WorkspaceWrite`]'s fixed arguments (0013). [`arguments`] adds the run's
/// [`worker_settings`] and `--add-dir` folders after them.
pub const WORKSPACE_WRITE_ARGS: &[&str] = &[
    "--restricted",
    "--tools",
    WORKER_TOOL_LIST,
    "--strict-mcp-config",
    "--permission-mode",
    "acceptEdits",
];

/// The oldest Claude Code that has every flag and setting a worker relies on: `--restricted`
/// arrived in 2.1.248, the last of them (0013). An older CLI rejects the unknown flag, and a
/// worker whose `system/init` reports an older version fails, but #156 also checks the detected
/// version before it starts one, for a clearer error.
pub const WORKER_MIN_VERSION: &str = "2.1.248";

/// Prefixes of inherited variables no run gets: Anthropic credentials, endpoints, profiles, and
/// federation (`ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`,
/// `ANTHROPIC_PROFILE`, ...), the cloud provider switches (`CLAUDE_CODE_USE_BEDROCK`, ...), and
/// OAuth tokens (`CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_OAUTH_REFRESH_TOKEN`).
pub const SCRUBBED_PREFIXES: &[&str] = &["ANTHROPIC_", "CLAUDE_CODE_USE_", "CLAUDE_CODE_OAUTH_"];

/// Inherited variables no run gets, besides [`SCRUBBED_PREFIXES`]: Bedrock's API key, and the
/// configuration folder, which [`apply_credential`] sets only to the account's own.
pub const SCRUBBED_VARS: &[&str] = &["AWS_BEARER_TOKEN_BEDROCK", CONFIG_DIR_ENV];

/// The variable that picks a second account's configuration folder.
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// What `system/init` reports as `apiKeySource` for a subscription login.
pub const SUBSCRIPTION_KEY_SOURCE: &str = "none";

/// The variable an API key account's key is injected as (0004's table, #118).
pub const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// What `system/init` reports as `apiKeySource` for an API key account. Happens to be the same
/// string as [`API_KEY_ENV`] (0004's table), but the two names are checked independently.
pub const API_KEY_SOURCE: &str = "ANTHROPIC_API_KEY";

/// Variables every run gets: keep credentials out of the agent's own subprocesses (0004
/// Consequences), and report a startup failure as a `result` instead of on stderr alone.
const ALWAYS_SET: &[(&str, &str)] = &[
    ("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "1"),
    ("CLAUDE_CODE_STARTUP_FAILURE_RESULTS", "1"),
];

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
}

/// The CLI's arguments for `request`.
///
/// # Errors
///
/// [`StartError::Invalid`] if the model or the resume id could be read as an option, or if a
/// worker has no usable [`WorkerSandbox`].
pub fn arguments(request: &RunRequest) -> Result<Vec<OsString>, StartError> {
    let policy = match request.policy {
        ToolPolicy::NoWrite => NO_WRITE_ARGS,
        ToolPolicy::WorkspaceWrite => WORKSPACE_WRITE_ARGS,
    };
    let mut args: Vec<OsString> = BASE_ARGS.iter().chain(policy).map(Into::into).collect();
    if let Some(sandbox) = worker_sandbox(request)? {
        let config_home = match &request.account.credential {
            Credential::Subscription { config_home } => config_home.as_deref(),
            Credential::ApiKey(_) => None,
        };
        let settings = worker_settings(sandbox, &request.cwd, config_home);
        args.extend(["--settings".into(), settings.to_string().into()]);
        for dir in &sandbox.writable {
            args.extend(["--add-dir".into(), dir.into()]);
        }
    }
    if let Some(model) = &request.model {
        check_value("model", model)?;
        args.extend(["--model".into(), model.into()]);
    }
    if let Some(resume) = &request.resume {
        check_value("resume id", &resume.session_id)?;
        args.extend(["--resume".into(), resume.session_id.clone().into()]);
    }
    Ok(args)
}

/// The `--settings` a worker runs with (0013): hooks off; the web tools allowed; and Claude Code's
/// Bash sandbox on, with no way around it, `sandbox`'s paths, and every host but
/// [`WORKER_DENIED_HOSTS`]. `WebFetch(domain:*)` is what opens the network: the sandbox takes its
/// allowlist from `WebFetch` allow rules, and a bare `*` matches every host. The denied hosts are
/// `WebFetch` deny rules as well as `deniedDomains`, because the sandbox's list binds only
/// commands, and a deny rule beats the `*` allow for the tool. `cwd`, the writable folders, and
/// the read-only git paths stay readable inside an unreadable path, such as wispd's data folder,
/// which holds the worktree, the context folder, and a normal thread's scratch repository
/// (#110). A second account's `config_home` is unreadable too.
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
            .chain(sandbox.read_only.iter().map(PathBuf::as_path)),
    );
    let read_only = strings(sandbox.read_only.iter().map(PathBuf::as_path));
    let denied_fetches: Vec<String> = WORKER_DENIED_HOSTS
        .iter()
        .map(|host| format!("WebFetch(domain:{host})"))
        .collect();
    serde_json::json!({
        "disableAllHooks": true,
        "permissions": {
            "allow": ["WebFetch(domain:*)", "WebSearch"],
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
        },
    })
}

/// Paths as settings strings. [`worker_sandbox`] has already refused any that isn't UTF-8.
fn strings<'a>(paths: impl Iterator<Item = &'a Path>) -> Vec<String> {
    paths
        .map(|path| path.to_string_lossy().into_owned())
        .collect()
}

fn check_value(what: &str, value: &str) -> Result<(), StartError> {
    if value.is_empty()
        || value.starts_with('-')
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(StartError::Invalid(format!(
            "the {what} {value:?} is not a usable argument"
        )));
    }
    Ok(())
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
            worker_sandbox: true,
        }
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        if request.prompt.is_empty() {
            return Err(StartError::Invalid("the prompt is empty".into()));
        }
        let mut spec = ProcessSpec::new(self.program.clone(), &request.cwd);
        spec.args = arguments(&request)?;
        spec.scrub = scrubbed(self.launcher.base());
        let expected_key_source = apply_credential(&request.account.credential, &mut spec)?;
        for (name, value) in ALWAYS_SET {
            spec.inject.set(name, value);
        }
        spec.stdin = StdinMode::Piped;
        spec.limits = self.limits;

        let process = self.launcher.spawn(&spec)?;
        let switch = CancelSwitch::new();
        switch.arm(process.signals().clone(), self.cancel);
        let (handle, control) = RunHandle::new(request.run_id, true, switch.clone());
        let stop = Arc::new(Notify::new());
        let baseline = request
            .resume
            .map(|resume| resume.usage_totals)
            .unwrap_or_default();
        let (sink, events) = EventSink::channel(EVENT_BUFFER, baseline);
        let driver = Driver {
            process,
            control,
            sink,
            switch,
            stop: Arc::clone(&stop),
            translator: Translator::new(request.policy, expected_key_source),
            turns: VecDeque::new(),
            violation: None,
        };
        tokio::spawn(driver.run(Message::new(request.turn_id, &request.prompt, false)));
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
    fn new(turn_id: Option<TurnId>, text: &str, follow_up: bool) -> Self {
        let uuid = turn_id.unwrap_or_else(TurnId::generate).to_string();
        let mut line = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": text},
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
}

/// The result of writing one message to stdin.
enum Delivery {
    Written(Message),
    Failed(Message),
}

/// Writes messages to stdin in order, off the driver's loop, so a CLI that stops reading stdin
/// can't keep the driver from reading its stdout.
async fn write_messages(
    mut stdin: pipe::Sender,
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
    sink: EventSink,
    switch: CancelSwitch,
    stop: Arc<Notify>,
    translator: Translator,
    /// Turns the CLI has been sent but hasn't finished, oldest first: their ids and `uuid`s.
    turns: VecDeque<(Option<TurnId>, String)>,
    violation: Option<Failure>,
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
                follow_up = self.control.recv(), if control_open => match follow_up {
                    Some(follow_up) => {
                        let message = Message::new(Some(follow_up.turn_id), &follow_up.text, true);
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
        let outcome = self.outcome(exit);
        let _ = self.sink.finish(outcome).await;
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
            }
        }
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

    /// Closes stdin once no turn is outstanding, so the CLI exits after its last result.
    fn close_when_idle(&mut self, stdin: &mut Stdin) {
        if stdin.is_open() && self.turns.is_empty() && stdin.pending == 0 {
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
            let message = Message::new(Some(follow_up.turn_id), &follow_up.text, true);
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
