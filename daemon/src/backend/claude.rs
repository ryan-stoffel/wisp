//! The Claude Code backend: runs the user's own signed-in `claude` CLI headless (0004, #116).
//!
//! # The command
//!
//! Every run is `claude -p --output-format stream-json --verbose --input-format stream-json` in
//! the run's cwd, plus the policy's flags, `--model`, and `--resume <session id>` (0004 [10]):
//!
//! - **No-write** is exactly 0004's: [`NO_WRITE_ARGS`]. As a second check, a no-write run whose
//!   `system/init` lists a write tool fails with [`FailureKind::PolicyViolation`].
//! - **Workspace-write** is [`WORKSPACE_WRITE_ARGS`], `--permission-mode acceptEdits`. That mode
//!   approves file edits, and `mkdir`, `touch`, `rm`, `rmdir`, `mv`, `cp`, and `sed`, only for
//!   paths inside the working directory, and never for protected paths. Any other call that would
//!   prompt is denied under `-p`, which has no one to ask (the permission modes and headless
//!   docs).
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
//! A subscription run scrubs [`SUBSCRIPTION_SCRUBBED`], which all outrank the CLI's own login,
//! and points `CLAUDE_CONFIG_DIR` at the account's configuration folder when it has one. A
//! project's `env` block can still set a key for a worker (0004), so every `system/init` is
//! checked: an `apiKeySource` other than `none`, or none at all, kills the CLI's process group at
//! once and fails the run with [`FailureKind::UnexpectedApiKey`]. API key runs are #118's, which
//! fills in [`apply_credential`].
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
use std::ffi::OsString;
use std::sync::Arc;
use std::time::Duration;

use rustix::process::Signal;
use tokio::io::AsyncWriteExt;
use tokio::net::unix::pipe;
use tokio::sync::{Notify, mpsc};

use self::stream::{Step, Translator, TurnDone};
use super::event::{Event, Failure, FailureKind, Outcome, WarningKind};
use super::process::{
    CancelPolicy, Exit, Launcher, Output, OutputLimits, Process, ProcessSpec, StdinMode,
};
use super::{
    Backend, CancelSwitch, Capabilities, Credential, EVENT_BUFFER, EventSink, FollowUp, Run,
    RunHandle, RunId, RunRequest, SendError, StartError, Started, ToolPolicy, TurnId,
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

/// [`ToolPolicy::WorkspaceWrite`]'s arguments: edits inside the working directory only.
pub const WORKSPACE_WRITE_ARGS: &[&str] = &["--permission-mode", "acceptEdits"];

/// Variables a subscription run doesn't pass on, since each outranks the CLI's login (0004).
pub const SUBSCRIPTION_SCRUBBED: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
];

/// The variable that picks a second account's configuration folder.
pub const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

/// What `system/init` reports as `apiKeySource` for a subscription login.
pub const SUBSCRIPTION_KEY_SOURCE: &str = "none";

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
/// [`StartError::Invalid`] if the model or the resume id could be read as an option.
pub fn arguments(request: &RunRequest) -> Result<Vec<OsString>, StartError> {
    let policy = match request.policy {
        ToolPolicy::NoWrite => NO_WRITE_ARGS,
        ToolPolicy::WorkspaceWrite => WORKSPACE_WRITE_ARGS,
    };
    let mut args: Vec<OsString> = BASE_ARGS.iter().chain(policy).map(Into::into).collect();
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

/// Sets up `spec`'s environment for `credential`, and returns the `apiKeySource` that
/// `system/init` must then report.
///
/// # Errors
///
/// [`StartError::Unsupported`] for an API key, until #118 injects it here as
/// `ANTHROPIC_API_KEY` and expects `apiKeySource` to be `ANTHROPIC_API_KEY`.
pub fn apply_credential(
    credential: &Credential,
    spec: &mut ProcessSpec,
) -> Result<&'static str, StartError> {
    match credential {
        Credential::Subscription { config_home } => {
            spec.scrub
                .extend(SUBSCRIPTION_SCRUBBED.iter().map(Into::into));
            if let Some(home) = config_home {
                spec.inject.set(CONFIG_DIR_ENV, home);
            }
            Ok(SUBSCRIPTION_KEY_SOURCE)
        }
        Credential::ApiKey(_) => Err(StartError::Unsupported(
            "this wispd can't run Claude Code with an API key yet".into(),
        )),
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
        }
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        if request.prompt.is_empty() {
            return Err(StartError::Invalid("the prompt is empty".into()));
        }
        let mut spec = ProcessSpec::new(self.program.clone(), &request.cwd);
        spec.args = arguments(&request)?;
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
        let count = match named {
            Some(index) => index + 1,
            // A CLI that echoes no ids: with nothing queued, every turn sent so far is done.
            None if done.uuids.is_empty() && done.queued == Some(0) => self.turns.len(),
            None => 1,
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
