//! The Cursor backend: runs the user's own signed-in Cursor Agent CLI, `agent`, as a thread's
//! full agent, the way Claude threads run full Claude Code (0034, 0036).
//!
//! # The command
//!
//! Every run is `agent [--model <id>] [--force] acp` in the run's cwd: Cursor Agent as an Agent
//! Client Protocol server, JSON-RPC 2.0 as one JSON object per line on stdin and stdout. The
//! driver sends `initialize`, then `session/new`, or `session/load` with the session to resume,
//! whose replayed history it drops ([`stream::Translator::replaying`]). Plan sends `session/set_mode
//! plan` next. Then each message is a `session/prompt`, the first one the user's message as
//! written, with its images as image blocks before its text. A follow-up sent during a turn waits
//! for that turn's response, then goes into the same session. Once no turn is outstanding and no
//! request waits, stdin closes, `agent` exits, and the run ends; a later message resumes the
//! session in a new run.
//!
//! Only threads run on Cursor (`RunRequest::thread`): it has no worker sandbox, and 0004 keeps the
//! coordinator off it, so anything else is [`StartError::Unsupported`].
//!
//! # Credentials and configuration
//!
//! Nothing is scrubbed from the user's `~/.cursor`: their login, rules, skills, MCP servers, and
//! allowlist load as in a terminal. Every run drops inherited variables starting with
//! [`SCRUBBED_PREFIX`], which could pick another key or endpoint (`CURSOR_API_KEY`,
//! `CURSOR_API_ENDPOINT`). Only the subscription runs: 0004 rules out `CURSOR_API_KEY` as a
//! fallback.
//!
//! # Permissions
//!
//! Edit is Cursor's `agent` mode, which edits without asking and asks before a command its
//! allowlist doesn't cover. Plan is `session/set_mode plan`, and Bypass is `--force`, which runs
//! everything. Cursor has no Manual, and `--auto-review` still asks for every command over ACP, so
//! Auto isn't offered either (0036). Effort is part of Cursor's model ids, so it maps no efforts.
//!
//! # Permission requests
//!
//! `session/request_permission` becomes [`Event::ApprovalRequested`], and [`Run::answer`]'s answer
//! selects its allow-once or reject-once option. ACP carries no message with a rejection, and no
//! edited input. A plan arrives as `cursor/create_plan`, which the app sees as an interactive
//! `ExitPlanMode` request. Allowing it accepts the plan, switches the session to `agent`, and
//! sends [`BUILD_PLAN`] as the same turn, as Claude Code goes on to build an approved plan in its
//! turn. Denying it rejects the plan with the user's message, and Cursor keeps planning. Without
//! `approvals`, plxd rejects every request and declines plans, which Cursor then saves itself.
//!
//! # Events
//!
//! Message chunks are text deltas; thinking chunks join into one `Reasoning`; tool calls are named
//! for the Claude Code tools the app draws (`Bash`, `Read`, `Edit`, `Grep`, `WebFetch`); todo and
//! plan updates are todo lists. Cursor reports no token usage over ACP, so a Cursor run has none.
//!
//! # Cancel
//!
//! `SIGINT` makes `agent acp` exit at once, so cancel sends it and closes stdin, and kills the
//! process group if it is still running after the grace period.

mod stream;
#[cfg(all(test, unix))]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::sync::{Notify, mpsc};

use self::stream::{Ask, AskKind, Step, Translator, permission_answer};
use super::commands::{self, CommandsProbe};
use super::event::{Event, Failure, FailureKind, Outcome, WarningKind};
use super::process::{
    CancelPolicy, Environment, Exit, Launcher, Output, Process, ProcessSpec, StdinMode, StdinPipe,
};
use super::{
    AgentPermission, Answer, AnswerError, ApprovalId, Backend, CancelSwitch, Capabilities,
    Credential, Decision, EVENT_BUFFER, EventSink, FollowUp, PromptImage, Run, RunHandle, RunId,
    RunRequest, SendError, StartError, Started, TurnId, check_argument,
};

/// The CLI's program name, looked up on the launcher's `PATH`.
pub const PROGRAM: &str = "agent";

/// The backend's name, which a subscription `AccountChoice` names.
pub const NAME: &str = "cursor";

/// The prefix of inherited variables no run gets: Cursor's key and endpoint.
pub const SCRUBBED_PREFIX: &str = "CURSOR_";

/// Cursor's ACP modes for Edit and Plan.
const AGENT_MODE: &str = "agent";
const PLAN_MODE: &str = "plan";

/// What Cursor maps (0036): Edit is its `agent` mode, Plan its `plan` mode, Bypass `--force`.
const PERMISSIONS: &[AgentPermission] = &[
    AgentPermission::Edit,
    AgentPermission::Plan,
    AgentPermission::Bypass,
];

/// The message that goes on with an approved plan in the same turn.
pub const BUILD_PLAN: &str = "The user approved the plan. Build it.";

/// A [`Backend`] that runs Cursor Agent.
#[derive(Clone, Debug)]
pub struct CursorBackend {
    launcher: Launcher,
}

impl CursorBackend {
    /// A backend that starts `agent` through `launcher`.
    #[must_use]
    pub fn new(launcher: Launcher) -> Self {
        Self { launcher }
    }

    /// `agent` in `cwd` with no arguments yet, the inherited [`SCRUBBED_PREFIX`] variables
    /// dropped, and stdin piped.
    fn spec(&self, cwd: &Path) -> ProcessSpec {
        let mut spec = ProcessSpec::new(PROGRAM, cwd);
        spec.scrub = scrubbed(self.launcher.base());
        spec.stdin = StdinMode::Piped;
        spec
    }
}

/// The ACP `initialize` params plxd sends: a client with no file system or terminal of its own.
fn initialize_params() -> Value {
    json!({
        "protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": "plxd", "version": crate::version()},
    })
}

/// The CLI's arguments for `request`.
///
/// # Errors
///
/// [`StartError::Unsupported`] for a run that isn't a thread, an API key account, an effort,
/// context window, or fast mode, or a permission Cursor doesn't map, and [`StartError::Invalid`]
/// for a model that could be read as an option.
pub fn arguments(request: &RunRequest) -> Result<Vec<OsString>, StartError> {
    if !request.thread {
        return Err(StartError::Unsupported(
            "plxd runs only threads on Cursor: it has no worker sandbox, and no coordinator (0036)"
                .into(),
        ));
    }
    match &request.account.credential {
        Credential::Subscription { config_home: None } => {}
        Credential::Subscription {
            config_home: Some(_),
        } => {
            return Err(StartError::Unsupported(
                "Cursor Agent has no folder for a second account".into(),
            ));
        }
        Credential::ApiKey(_) => {
            return Err(StartError::Unsupported(
                "Cursor runs only on its signed-in subscription, never an API key (0004)".into(),
            ));
        }
    }
    if request.effort.is_some() || request.context_window.is_some() || request.fast.is_some() {
        return Err(StartError::Unsupported(
            "Cursor's effort, context window, and speed are part of its model id".into(),
        ));
    }
    let mut args: Vec<OsString> = Vec::new();
    if let Some(model) = &request.model {
        check_argument("model", model)?;
        args.extend(["--model".into(), model.into()]);
    }
    match request.permission {
        None | Some(AgentPermission::Edit | AgentPermission::Plan) => {}
        Some(AgentPermission::Bypass) => args.push("--force".into()),
        Some(_) => {
            return Err(StartError::Unsupported(
                "Cursor has no mode for this permission".into(),
            ));
        }
    }
    args.push("acp".into());
    Ok(args)
}

/// The variables of `base` that no run gets: [`SCRUBBED_PREFIX`].
#[must_use]
pub fn scrubbed(base: &Environment) -> Vec<OsString> {
    base.names()
        .filter(|name| {
            name.as_encoded_bytes()
                .starts_with(SCRUBBED_PREFIX.as_bytes())
        })
        .map(OsStr::to_owned)
        .collect()
}

impl Backend for CursorBackend {
    fn name(&self) -> &'static str {
        NAME
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            follow_ups: true,
            resume: true,
            coordinator: false,
            reports_cost: false,
            rate_limits: false,
            worker_sandbox: false,
            fork: false,
        }
    }

    fn permissions(&self) -> &'static [AgentPermission] {
        PERMISSIONS
    }

    /// `agent acp`, with a new session for `cwd` ([`commands::cursor`]).
    fn commands(&self, cwd: &Path) -> Result<Option<CommandsProbe>, StartError> {
        let mut spec = self.spec(cwd);
        spec.args = vec!["acp".into()];
        Ok(Some(CommandsProbe {
            process: self.launcher.spawn(&spec)?,
            input: vec![
                commands::request(1, "initialize", &initialize_params()),
                commands::request(
                    commands::LIST_ID,
                    "session/new",
                    &json!({"cwd": cwd, "mcpServers": []}),
                ),
            ],
            parse: commands::cursor,
        }))
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        if request.resume.as_ref().is_some_and(|resume| resume.fork) {
            return Err(StartError::Unsupported(
                "Cursor Agent can't fork a session".into(),
            ));
        }
        if request.prompt.is_empty() && request.images.is_empty() {
            return Err(StartError::Invalid("the prompt is empty".into()));
        }
        let mut spec = self.spec(&request.cwd);
        spec.args = arguments(&request)?;
        let mut process = self.launcher.spawn(&spec)?;

        let switch = CancelSwitch::new();
        switch.arm(process.signals().clone(), CancelPolicy::default());
        let (handle, control) = RunHandle::new(request.run_id, true, switch.clone());
        let (handle, answers) = handle.with_answers();
        let stop = Arc::new(Notify::new());
        let baseline = request
            .resume
            .as_ref()
            .map(|resume| resume.usage_totals.clone())
            .unwrap_or_default();
        let (sink, events) = EventSink::channel(EVENT_BUFFER, baseline);
        let (stdin, lines) = mpsc::unbounded_channel();
        let writer = process
            .take_stdin()
            .map(|pipe| tokio::spawn(write_lines(pipe, lines)));
        let mut translator = Translator::default();
        translator.asks = request.approvals;
        let driver = Driver {
            process,
            control,
            answers,
            sink,
            switch,
            stop: Arc::clone(&stop),
            translator,
            stdin: Some(stdin),
            writer,
            next_id: 0,
            requests: HashMap::new(),
            cwd: request.cwd.to_string_lossy().into_owned(),
            resume: request.resume.map(|resume| resume.session_id),
            plan: request.permission == Some(AgentPermission::Plan),
            session: None,
            modes_pending: 0,
            prompts: VecDeque::from([Prompt::new(
                request.turn_id,
                &request.prompt,
                &request.images,
                false,
            )]),
            in_flight: None,
            build: None,
            asks: HashMap::new(),
            failure: None,
            results: 0,
            last_result: None,
        };
        tokio::spawn(driver.run());
        Ok(Started {
            run: Arc::new(CursorRun { handle, stop }),
            events,
        })
    }
}

/// The run's handle: [`RunHandle`], plus closing stdin on cancel.
struct CursorRun {
    handle: RunHandle,
    stop: Arc<Notify>,
}

impl Run for CursorRun {
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

/// A message for `session/prompt`.
#[derive(Debug)]
struct Prompt {
    turn_id: Option<TurnId>,
    content: Vec<Value>,
    /// A follow-up, which is reported dropped if the CLI exits before taking it.
    follow_up: bool,
    /// Its `TurnStarted` was already sent: an approved plan's build, which goes on its turn.
    started: bool,
}

impl Prompt {
    /// `text` as a text block, after `images` as ACP image blocks. Images alone have no text block.
    fn new(turn_id: Option<TurnId>, text: &str, images: &[PromptImage], follow_up: bool) -> Self {
        let images = images.iter().map(
            |image| json!({"type": "image", "mimeType": image.media_type, "data": image.data}),
        );
        let text = (!text.trim().is_empty()).then(|| json!({"type": "text", "text": text}));
        Self {
            turn_id,
            content: images.chain(text).collect(),
            follow_up,
            started: false,
        }
    }
}

/// What one of plxd's requests was, for its response.
#[derive(Debug)]
enum Request {
    Initialize,
    Session,
    Mode,
    Prompt(Prompt),
}

/// Writes lines to stdin in order, off the driver's loop, and closes it once every sender is gone.
async fn write_lines(mut stdin: StdinPipe, mut lines: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = lines.recv().await {
        if stdin.write_all(line.as_bytes()).await.is_err() {
            break;
        }
    }
}

/// One run: speaks ACP with the CLI, delivers follow-ups and answers, and decides the outcome
/// when the CLI exits.
struct Driver {
    process: Process,
    control: mpsc::UnboundedReceiver<FollowUp>,
    answers: mpsc::UnboundedReceiver<Answer>,
    sink: EventSink,
    switch: CancelSwitch,
    stop: Arc<Notify>,
    translator: Translator,
    /// Lines for [`write_lines`]; `None` once stdin is closed.
    stdin: Option<mpsc::UnboundedSender<String>>,
    writer: Option<tokio::task::JoinHandle<()>>,
    next_id: u64,
    /// plxd's requests that haven't been answered, by id.
    requests: HashMap<u64, Request>,
    cwd: String,
    resume: Option<String>,
    plan: bool,
    /// The session's id, once `session/new` or `session/load` answered.
    session: Option<String>,
    /// `session/set_mode` requests not yet answered; prompts wait for them.
    modes_pending: usize,
    /// Messages waiting for the session, or for the turn before them to end.
    prompts: VecDeque<Prompt>,
    /// The id of the `session/prompt` the CLI is working on.
    in_flight: Option<u64>,
    /// An approved plan's build, which goes on the plan's turn once the CLI ends it.
    build: Option<String>,
    /// Requests the CLI waits on.
    asks: HashMap<ApprovalId, Ask>,
    failure: Option<Failure>,
    results: usize,
    last_result: Option<String>,
}

impl Driver {
    async fn run(mut self) {
        self.request("initialize", &initialize_params(), Request::Initialize);
        let mut control_open = true;
        let mut answers_open = true;
        let exit = loop {
            tokio::select! {
                output = self.process.next() => match output {
                    Some(Output::Line(line)) => {
                        let steps = self.translator.line(&line);
                        self.apply(steps).await;
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
                answer = self.answers.recv(), if answers_open => match answer {
                    Some(answer) => self.answer(answer).await,
                    None => answers_open = false,
                },
                follow_up = self.control.recv(), if control_open => match follow_up {
                    Some(follow_up) => {
                        let prompt = Prompt::new(Some(follow_up.turn_id), &follow_up.text, &follow_up.images, true);
                        self.prompts.push_back(prompt);
                    }
                    None => control_open = false,
                },
                () = self.stop.notified(), if self.stdin.is_some() => self.close(),
                () = self.sink.closed(), if !self.switch.is_cancelled() => {
                    self.switch.cancel();
                    self.close();
                }
            }
            self.next_prompt().await;
            if self.stdin.is_some()
                && self.session.is_some()
                && self.in_flight.is_none()
                && self.prompts.is_empty()
                && self.asks.is_empty()
                && self.build.is_none()
            {
                self.close();
            }
        };

        if let Some(step) = self.translator.flush() {
            self.apply(vec![step]).await;
        }
        self.drop_undelivered().await;
        for approval_id in std::mem::take(&mut self.asks).into_keys() {
            self.emit(Event::ApprovalWithdrawn { approval_id }).await;
        }
        let outcome = self.outcome(exit);
        let _ = self.sink.finish(outcome).await;
    }

    /// Sends a JSON-RPC request, and remembers what it was for its response.
    fn request(&mut self, method: &str, params: &Value, kind: Request) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        self.write(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.requests.insert(id, kind);
        id
    }

    fn write(&mut self, message: &Value) {
        if let Some(stdin) = &self.stdin {
            let _ = stdin.send(format!("{message}\n"));
        }
    }

    /// Closes stdin once what is queued is written, so the CLI exits after its turn; no more
    /// follow-ups are taken.
    fn close(&mut self) {
        self.stdin = None;
        self.control.close();
    }

    /// Sends the next message once the session is ready and no turn is in flight.
    async fn next_prompt(&mut self) {
        let Some(session) = self.session.clone() else {
            return;
        };
        if self.in_flight.is_some() || self.modes_pending > 0 || self.stdin.is_none() {
            return;
        }
        let Some(prompt) = self.prompts.pop_front() else {
            return;
        };
        let (turn_id, started) = (prompt.turn_id, prompt.started);
        let params = json!({"sessionId": session, "prompt": prompt.content});
        self.in_flight = Some(self.request("session/prompt", &params, Request::Prompt(prompt)));
        if !started {
            self.emit(Event::TurnStarted { turn_id }).await;
        }
    }

    async fn apply(&mut self, steps: Vec<Step>) {
        for step in steps {
            match step {
                Step::Emit(event) => self.emit(event).await,
                Step::Reply(message) => self.write(&message),
                Step::Ask(request, ask) => {
                    self.asks.insert(request.approval_id, ask);
                    self.emit(Event::ApprovalRequested(request)).await;
                }
                Step::Response { id, result } => self.response(id, result).await,
            }
        }
    }

    async fn response(&mut self, id: u64, result: Result<Value, String>) {
        let Some(request) = self.requests.remove(&id) else {
            return;
        };
        let value = match result {
            Ok(value) => value,
            Err(message) => {
                if matches!(request, Request::Mode) {
                    self.modes_pending = self.modes_pending.saturating_sub(1);
                    self.emit(Event::Notice { detail: message }).await;
                    return;
                }
                // The session or a turn failed: the run ends with it. Every prompt's turn has
                // started by now, an approved plan's build on the plan's turn.
                if let Request::Prompt(prompt) = &request {
                    let result = None;
                    let turn_id = prompt.turn_id;
                    self.emit(Event::TurnFinished { turn_id, result }).await;
                }
                self.failure = Some(failure(classify(&message), message));
                self.in_flight = None;
                self.close();
                return;
            }
        };
        match request {
            Request::Initialize => {
                let params = json!({"cwd": self.cwd, "mcpServers": []});
                match self.resume.clone() {
                    Some(session) => {
                        self.translator.replaying = true;
                        let params =
                            json!({"sessionId": session, "cwd": self.cwd, "mcpServers": []});
                        self.request("session/load", &params, Request::Session);
                    }
                    None => {
                        self.request("session/new", &params, Request::Session);
                    }
                }
            }
            Request::Session => {
                self.translator.replaying = false;
                let session = value
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .or_else(|| self.resume.clone());
                let Some(session) = session else {
                    self.failure = Some(failure(
                        FailureKind::VendorError,
                        "Cursor Agent started no session".into(),
                    ));
                    self.close();
                    return;
                };
                let model = value
                    .pointer("/models/currentModelId")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.emit(Event::SessionStarted {
                    session_id: session.clone(),
                    model,
                    api_key_source: None,
                })
                .await;
                if self.plan {
                    self.set_mode(&session, PLAN_MODE);
                }
                self.session = Some(session);
            }
            Request::Mode => self.modes_pending = self.modes_pending.saturating_sub(1),
            Request::Prompt(prompt) => {
                self.in_flight = None;
                if value.get("stopReason").and_then(Value::as_str) == Some("cancelled") {
                    self.build = None;
                }
                if let Some(text) = self.build.take() {
                    // The approved plan's build goes on as this turn.
                    self.prompts.push_front(Prompt {
                        turn_id: prompt.turn_id,
                        content: vec![json!({"type": "text", "text": text})],
                        follow_up: false,
                        started: true,
                    });
                    return;
                }
                self.results += 1;
                let result = self.translator.take_text();
                self.last_result.clone_from(&result);
                let turn_id = prompt.turn_id;
                self.emit(Event::TurnFinished { turn_id, result }).await;
            }
        }
    }

    fn set_mode(&mut self, session: &str, mode: &str) {
        self.modes_pending += 1;
        let params = json!({"sessionId": session, "modeId": mode});
        self.request("session/set_mode", &params, Request::Mode);
    }

    /// Writes `answer` to the CLI, if it still waits on the request. A denial that interrupts
    /// also cancels the turn, and withdraws every other request it waits on, answered `cancelled`
    /// as ACP has a client do after `session/cancel`.
    async fn answer(&mut self, answer: Answer) {
        let Some(ask) = self.asks.remove(&answer.approval_id) else {
            return;
        };
        let interrupt = matches!(
            answer.decision,
            Decision::Deny {
                interrupt: true,
                ..
            }
        );
        let leaves_plan = matches!(
            (&ask.kind, &answer.decision),
            (AskKind::Plan { .. }, Decision::Allow { .. })
        );
        let reply = match (ask.kind, answer.decision) {
            (AskKind::Permission { allow, .. }, Decision::Allow { .. }) => {
                permission_answer(&ask.id, allow.as_deref())
            }
            (AskKind::Permission { reject, .. }, Decision::Deny { .. }) => {
                if let Some(call_id) = ask.call_id {
                    self.translator.denied.insert(call_id);
                }
                permission_answer(&ask.id, reject.as_deref())
            }
            (AskKind::Plan { plan }, Decision::Allow { input, .. }) => {
                // An edited plan is the user's own: the build says so, since Cursor's answer has
                // no field for it.
                let edited = input
                    .as_ref()
                    .and_then(|input| input.get("plan"))
                    .and_then(Value::as_str)
                    .filter(|edited| *edited != plan);
                self.build = Some(match edited {
                    Some(edited) => format!("{BUILD_PLAN} Use this version of it:\n\n{edited}"),
                    None => BUILD_PLAN.to_owned(),
                });
                json!({"jsonrpc": "2.0", "id": ask.id, "result": {"outcome": {"outcome": "accepted"}}})
            }
            (AskKind::Plan { .. }, Decision::Deny { message, .. }) => {
                if let Some(call_id) = ask.call_id {
                    self.translator.denied.insert(call_id);
                }
                let mut outcome = json!({"outcome": "rejected"});
                if !message.trim().is_empty() {
                    outcome["reason"] = Value::from(message);
                }
                json!({"jsonrpc": "2.0", "id": ask.id, "result": {"outcome": outcome}})
            }
        };
        self.write(&reply);
        // Out of plan mode, after the answer, for the build.
        if leaves_plan && let Some(session) = self.session.clone() {
            self.set_mode(&session, AGENT_MODE);
        }
        if interrupt && let Some(session) = self.session.clone() {
            self.write(&json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session}}));
            for (approval_id, ask) in std::mem::take(&mut self.asks) {
                let outcome = json!({"outcome": "cancelled"});
                self.write(
                    &json!({"jsonrpc": "2.0", "id": ask.id, "result": {"outcome": outcome}}),
                );
                self.emit(Event::ApprovalWithdrawn { approval_id }).await;
            }
        }
    }

    async fn emit(&mut self, event: Event) {
        if self.sink.emit(event).await.is_err() {
            self.switch.cancel();
        }
    }

    /// After the CLI exited: reports every follow-up that never started a turn as dropped.
    async fn drop_undelivered(&mut self) {
        self.close();
        let mut dropped: Vec<TurnId> = std::mem::take(&mut self.prompts)
            .into_iter()
            .filter(|prompt| prompt.follow_up)
            .filter_map(|prompt| prompt.turn_id)
            .collect();
        while let Ok(follow_up) = self.control.try_recv() {
            dropped.push(follow_up.turn_id);
        }
        for turn_id in dropped {
            self.emit(Event::FollowUpDropped { turn_id }).await;
        }
        if let Some(writer) = self.writer.take() {
            let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
        }
    }

    fn outcome(&mut self, exit: Option<Exit>) -> Outcome {
        if self.switch.is_cancelled() {
            return Outcome::Cancelled;
        }
        if let Some(failure) = self.failure.take() {
            return failed(failure, exit.as_ref());
        }
        let Some(exit) = exit else {
            return failed(
                failure(FailureKind::Internal, "lost track of the process".into()),
                None,
            );
        };
        if exit.info.success() && self.results > 0 {
            return Outcome::Completed {
                result: self.last_result.take(),
            };
        }
        let failure = match classify(&exit.stderr_tail) {
            FailureKind::NotSignedIn => failure(
                FailureKind::NotSignedIn,
                "Cursor Agent is not signed in".into(),
            ),
            _ if exit.info.success() => failure(
                FailureKind::VendorError,
                "Cursor Agent exited without finishing its turn".into(),
            ),
            _ => {
                let message = match (exit.info.code, exit.info.signal) {
                    (_, Some(signal)) => format!("Cursor Agent was killed by signal {signal}"),
                    (Some(code), None) => format!("Cursor Agent exited with code {code}"),
                    (None, None) => "Cursor Agent ended in an unknown way".to_owned(),
                };
                failure(FailureKind::Crashed, message)
            }
        };
        failed(failure, Some(&exit))
    }
}

/// What kind of failure Cursor's message describes: its "Authentication required" error, or a
/// usage limit, which routing falls back on (0012).
fn classify(message: &str) -> FailureKind {
    let lower = message.to_ascii_lowercase();
    if lower.contains("authentication required")
        || lower.contains("not logged in")
        || lower.contains("agent login")
    {
        FailureKind::NotSignedIn
    } else if lower.contains("rate limit") || lower.contains("usage limit") {
        FailureKind::RateLimited
    } else {
        FailureKind::VendorError
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
