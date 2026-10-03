//! A Codex thread (0017, 0035): the user's own `codex app-server`, as full Codex.
//!
//! # The command
//!
//! `codex app-server` in the run's cwd, which speaks JSON-RPC over stdio, one message per line.
//! It loads the user's `config.toml`, rules, `AGENTS.md` files, skills, hooks, plugins, and MCP
//! servers, as `codex` in a terminal does: no `--ignore-user-config`, `--ignore-rules`, or
//! permission profile, unlike a worker's `codex exec`. The driver sends `initialize` and
//! `initialized`, then `thread/start`, or `thread/resume` with the earlier run's thread id, with
//! the model, the context window as `model_context_window`, fast mode as the `priority` service
//! tier, and the mode's approval policy and sandbox ([`mode`]). The prompt is the first
//! `turn/start`, as written, with its images as `localImage` files ([`write_images`]) and the
//! effort. Each follow-up is a later `turn/start` in the same process, sent once the turn before
//! it has completed. A steer (PLX-370) is `turn/steer` with the running turn's id, which
//! codex-cli 0.160.0 adds to that turn's input after its current item; if Codex refuses it, or no
//! turn runs, it is the next turn instead. Once no turn, steer, or approval request is
//! outstanding and plxd holds no message for it ([`Run::hold`](super::super::Run::hold)), stdin
//! closes and app-server exits, which ends the run; `agent/send` then resumes the thread in a new
//! run.
//!
//! # Approval requests
//!
//! Codex asks the client before what its approval policy doesn't allow: a command
//! (`item/commandExecution/requestApproval`), a patch (`item/fileChange/requestApproval`), more
//! sandbox permissions (`item/permissions/requestApproval`), or an MCP server's question
//! (`mcpServer/elicitation/request`). With [`RunRequest::approvals`], each is an
//! [`Event::ApprovalRequested`], and [`Run::answer`](super::super::Run::answer)'s answer is the
//! request's JSON-RPC response ([`translate::answer_response`]). `serverRequest/resolved` for a
//! request still waiting, or app-server exiting, withdraws it. Without `approvals`, the thread
//! runs with approval policy `never` inside the mode's sandbox, so its commands run sandboxed
//! without asking, and plxd declines any request that still comes. Codex's other requests get a
//! JSON-RPC error, so it never waits on plxd.
//!
//! # Credentials
//!
//! As for exec, the run drops every inherited [`SCRUBBED_PREFIXES`] variable, and a subscription
//! gets only its account's `CODEX_HOME`, if it has one. app-server reads no API key from the
//! environment (checked with 0.159.3: `account/read` finds no account with `CODEX_API_KEY` or
//! `OPENAI_API_KEY` set), so a thread on an API key account is refused rather than billed to the
//! login.
//!
//! # Cancel
//!
//! app-server ignores `SIGINT`, so cancel sends `SIGTERM`, which ends it at once, and kills the
//! process group if it is still running after the grace period. The thread keeps what it had
//! written, so a later run resumes it.

#[cfg(all(test, unix))]
mod tests;
mod translate;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;

use self::translate::{Ask, Step, Translator, answer_response, refusal};
use super::{CONFIG_DIR_ENV, CONTEXT_WINDOWS, PROGRAM, effort_level, scrubbed, write_images};
use crate::backend::event::{Event, Failure, FailureKind, Outcome, WarningKind};
use crate::backend::process::{
    CancelPolicy, Exit, Launcher, Output, Process, ProcessSpec, Signal, SpawnError, StdinMode,
    StdinPipe,
};
use crate::backend::{
    AgentPermission, Answer, ApprovalId, CancelSwitch, Credential, Decision, EVENT_BUFFER,
    EventSink, FollowUp, Held, RunHandle, RunRequest, StartError, Started, TurnId, check_argument,
};

/// The permissions a thread maps, in Claude Code's picker order (0027): Codex's own presets
/// ([`mode`]). Plan is Codex's experimental collaboration mode, which plxd doesn't run (0035).
pub const PERMISSIONS: &[AgentPermission] = &[
    AgentPermission::Auto,
    AgentPermission::Manual,
    AgentPermission::Edit,
    AgentPermission::Bypass,
];

/// The approval policy, sandbox mode, and approvals reviewer a thread in `permission` runs with:
///
/// | Parallax | `approvalPolicy` | `sandbox` | `approvalsReviewer` |
/// | --- | --- | --- | --- |
/// | Manual | `untrusted` | `workspace-write` | |
/// | Accept Edits (the default) | `on-request` | `workspace-write` | |
/// | Auto | `on-request` | `workspace-write` | `auto_review` |
/// | Bypass Permissions | `never` | `danger-full-access` | |
///
/// # Errors
///
/// [`StartError::Unsupported`] for Plan or a permission this version doesn't know.
pub fn mode(
    permission: Option<AgentPermission>,
) -> Result<(&'static str, &'static str, Option<&'static str>), StartError> {
    match permission {
        None | Some(AgentPermission::Edit) => Ok(("on-request", "workspace-write", None)),
        Some(AgentPermission::Manual) => Ok(("untrusted", "workspace-write", None)),
        Some(AgentPermission::Auto) => Ok(("on-request", "workspace-write", Some("auto_review"))),
        Some(AgentPermission::Bypass) => Ok(("never", "danger-full-access", None)),
        Some(AgentPermission::Plan | AgentPermission::Unknown) => Err(StartError::Unsupported(
            "a Codex thread has no plan mode (decision 0035)".into(),
        )),
    }
}

/// Starts `request`, a thread's run, on `launcher`'s `codex app-server`.
///
/// # Errors
///
/// [`StartError::Unsupported`] for an API key account, a permission [`mode`] doesn't map, or an
/// effort or context window Codex doesn't know; [`StartError::Invalid`] for an empty message or a
/// model or resume id that can't be an argument; and the spawn's error.
pub(super) fn start(launcher: &Launcher, request: RunRequest) -> Result<Started, StartError> {
    if request.prompt.is_empty() && request.images.is_empty() {
        return Err(StartError::Invalid("the prompt is empty".into()));
    }
    let Credential::Subscription { config_home } = &request.account.credential else {
        return Err(StartError::Unsupported(
            "a Codex thread runs on a Codex login; app-server can't take an API key (decision \
             0035)"
                .into(),
        ));
    };
    let (thread_method, thread) = thread_params(&request)?;
    let effort = request.effort.map(effort_level).transpose()?;
    let temp_dir = launcher.data_dir().temp_dir();
    let (images, image_paths) = write_images(&temp_dir, &request.images)
        .map_err(SpawnError::Io)?
        .unzip();

    let mut process = launcher.spawn(&spec(launcher, &request.cwd, config_home.as_deref()))?;

    let switch = CancelSwitch::new();
    let policy = CancelPolicy {
        signal: Signal::TERM,
        ..CancelPolicy::default()
    };
    switch.arm(process.signals().clone(), policy);
    let (handle, control) = RunHandle::new(request.run_id, true, switch.clone());
    let (handle, answers) = handle.with_answers();
    let held = handle.held();
    let baseline = request
        .resume
        .map(|resume| resume.usage_totals)
        .unwrap_or_default();
    let (sink, events) = EventSink::channel(EVENT_BUFFER, baseline);
    let first = Turn {
        turn_id: request.turn_id,
        input: input(&request.prompt, image_paths.as_deref().unwrap_or_default()),
    };
    let driver = Driver {
        stdin: Stdin::start(&mut process),
        process,
        control,
        answers,
        held,
        sink,
        switch,
        translator: Translator::default(),
        approvals: request.approvals,
        effort,
        thread: Some((thread_method, thread)),
        thread_id: None,
        next_id: 0,
        requests: HashMap::new(),
        running: None,
        queued: VecDeque::from([first]),
        started_any: false,
        asks: HashMap::new(),
        temp_dir,
        images: images.into_iter().collect(),
        failure: None,
        last_result: None,
        turns_done: 0,
    };
    tokio::spawn(driver.run());
    Ok(Started {
        run: Arc::new(handle),
        events,
    })
}

/// `codex app-server` in `cwd`, as a thread on the subscription whose configuration folder is
/// `config_home` (the default one when absent) runs it: the inherited [`SCRUBBED_PREFIXES`]
/// variables dropped, and stdin piped.
pub(super) fn spec(launcher: &Launcher, cwd: &Path, config_home: Option<&Path>) -> ProcessSpec {
    let mut spec = ProcessSpec::new(PROGRAM, cwd);
    spec.args = vec!["app-server".into()];
    spec.scrub = scrubbed(launcher.base());
    if let Some(home) = config_home {
        spec.inject.set(CONFIG_DIR_ENV, home);
    }
    spec.stdin = StdinMode::Piped;
    spec
}

/// The JSON-RPC `initialize` params plxd sends app-server.
pub(super) fn initialize_params() -> Value {
    let client = json!({"name": "plxd", "title": "Parallax", "version": env!("CARGO_PKG_VERSION")});
    json!({"clientInfo": client, "capabilities": null})
}

/// `thread/start`, or `thread/resume` for a run that resumes a thread, and its params: the cwd,
/// the mode ([`mode`]), the model, the context window, and fast mode.
fn thread_params(request: &RunRequest) -> Result<(&'static str, Value), StartError> {
    let (mut approval_policy, sandbox, mut reviewer) = mode(request.permission)?;
    // A client that can't show a request: Codex's own sandbox holds the thread, and nothing asks,
    // as a Claude thread without `approvals` keeps its sandbox (0034).
    if !request.approvals {
        (approval_policy, reviewer) = ("never", None);
    }
    if let Some(model) = &request.model {
        check_argument("model", model)?;
    }
    let mut thread = json!({
        "cwd": request.cwd,
        "approvalPolicy": approval_policy,
        "sandbox": sandbox,
        "model": request.model,
    });
    if let Some(reviewer) = reviewer {
        thread["approvalsReviewer"] = reviewer.into();
    }
    if let Some(tokens) = request.context_window {
        if !CONTEXT_WINDOWS.contains(&tokens) {
            return Err(StartError::Unsupported(format!(
                "Codex has no {tokens}-token context window"
            )));
        }
        thread["config"] = json!({ "model_context_window": tokens });
    }
    if let Some(fast) = request.fast {
        // The catalog's tier named "Fast" is `priority`, as for exec.
        thread["serviceTier"] = if fast { "priority" } else { "default" }.into();
    }
    let thread_method = match &request.resume {
        Some(resume) => {
            check_argument("resume id", &resume.session_id)?;
            thread["threadId"] = resume.session_id.clone().into();
            thread["excludeTurns"] = true.into();
            "thread/resume"
        }
        None => "thread/start",
    };
    Ok((thread_method, thread))
}

/// A turn's `input`: its images as `localImage` files, then its text, if any.
fn input(text: &str, images: &[PathBuf]) -> Value {
    let images = images
        .iter()
        .map(|path| json!({"type": "localImage", "path": path}));
    let text = (!text.trim().is_empty())
        .then(|| json!({"type": "text", "text": text, "text_elements": []}));
    images.chain(text).collect()
}

/// A turn to start: the caller's id for it and its input.
#[derive(Debug)]
struct Turn {
    turn_id: Option<TurnId>,
    input: Value,
}

/// The turn Codex is running: its caller's id, if it has one, Codex's own once `turn/start` has
/// answered, and the messages steered into it, whose turns end with it.
#[derive(Debug)]
struct Running {
    turn_id: Option<TurnId>,
    codex_id: Option<String>,
    steered: Vec<TurnId>,
}

/// What one of plxd's requests was, to read its response.
#[derive(Debug)]
enum Request {
    Initialize,
    Thread,
    Turn,
    /// A `turn/steer` with the message it carries, which becomes the next turn if Codex refuses.
    Steer(Turn),
}

/// Writes lines to app-server's stdin in order, off the driver's loop, so an app-server that
/// stops reading can't keep the driver from reading its stdout. Dropping the queue closes stdin
/// once what is queued is written.
struct Stdin {
    queue: Option<mpsc::UnboundedSender<String>>,
}

impl Stdin {
    fn start(process: &mut Process) -> Self {
        let Some(pipe) = process.take_stdin() else {
            return Self { queue: None };
        };
        let (queue, lines) = mpsc::unbounded_channel();
        tokio::spawn(write_lines(pipe, lines));
        Self { queue: Some(queue) }
    }

    fn send(&self, message: &Value) {
        if let Some(queue) = &self.queue {
            let _ = queue.send(format!("{message}\n"));
        }
    }

    fn close(&mut self) {
        self.queue = None;
    }
}

async fn write_lines(mut pipe: StdinPipe, mut lines: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = lines.recv().await {
        if pipe.write_all(line.as_bytes()).await.is_err() {
            break;
        }
    }
}

/// One thread's app-server: forwards its events, runs its turns one at a time, relays approval
/// requests, and decides the outcome when it exits.
struct Driver {
    process: Process,
    stdin: Stdin,
    control: mpsc::UnboundedReceiver<FollowUp>,
    answers: mpsc::UnboundedReceiver<Answer>,
    /// While held, plxd has a message waiting for Codex, so stdin stays open (PLX-370).
    held: Held,
    sink: EventSink,
    switch: CancelSwitch,
    translator: Translator,
    approvals: bool,
    effort: Option<&'static str>,
    /// `thread/start` or `thread/resume` and its params, until it is sent.
    thread: Option<(&'static str, Value)>,
    /// Codex's thread id, once `thread/start` or `thread/resume` answered.
    thread_id: Option<String>,
    next_id: u64,
    requests: HashMap<u64, Request>,
    /// The turn Codex is running.
    running: Option<Running>,
    /// Turns waiting for the one running, oldest first. The first is the prompt's.
    queued: VecDeque<Turn>,
    /// A turn was sent: the prompt's.
    started_any: bool,
    /// Approval requests Codex waits on.
    asks: HashMap<ApprovalId, Ask>,
    /// Where follow-ups' images go.
    temp_dir: PathBuf,
    /// The turns' image folders, deleted once app-server has exited.
    images: Vec<TempDir>,
    /// Why the last turn, or the thread's start, failed.
    failure: Option<Failure>,
    last_result: Option<String>,
    turns_done: usize,
}

impl Driver {
    async fn run(mut self) {
        let first = self.queued.front().map(|turn| turn.turn_id);
        if let Some(turn_id) = first {
            self.emit(Event::TurnStarted { turn_id }).await;
        }
        self.request(Request::Initialize, "initialize", &initialize_params());
        let mut control_open = true;
        let mut answers_open = true;
        let exit = loop {
            tokio::select! {
                biased;
                output = self.process.next() => match output {
                    Some(Output::Line(line)) => {
                        for step in self.translator.line(&line) {
                            self.apply(step).await;
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
                // Answers before follow-ups: Codex is waiting on them.
                answer = self.answers.recv(), if answers_open => match answer {
                    Some(answer) => self.answer(&answer),
                    None => answers_open = false,
                },
                follow_up = self.control.recv(), if control_open => match follow_up {
                    Some(follow_up) => self.follow_up(follow_up).await,
                    None => control_open = false,
                },
                () = self.sink.closed(), if !self.switch.is_cancelled() => {
                    self.switch.cancel();
                    self.stdin.close();
                    self.control.close();
                }
                () = self.held.changed() => {}
            }
            self.close_when_idle();
        };
        self.drop_undelivered().await;
        for approval_id in std::mem::take(&mut self.asks).into_keys() {
            self.emit(Event::ApprovalWithdrawn { approval_id }).await;
        }
        self.images.clear();
        let outcome = self.outcome(exit);
        let _ = self.sink.finish(outcome).await;
    }

    fn request(&mut self, kind: Request, method: &str, params: &Value) {
        self.next_id += 1;
        self.requests.insert(self.next_id, kind);
        self.stdin
            .send(&json!({"id": self.next_id, "method": method, "params": params}));
    }

    async fn apply(&mut self, step: Step) {
        match step {
            Step::Emit(event) => self.emit(event).await,
            Step::Total(total) => {
                if self.sink.observe_total(None, total).await.is_err() {
                    self.switch.cancel();
                }
            }
            Step::Reply { id, result } => self.reply(id, result).await,
            Step::TurnDone { result, failure } => {
                self.turns_done += 1;
                self.last_result.clone_from(&result);
                self.failure = failure;
                self.finish_running(result).await;
                self.next_turn().await;
            }
            Step::Ask(request, ask) => {
                if self.approvals {
                    self.asks.insert(request.approval_id, ask);
                    self.emit(Event::ApprovalRequested(request)).await;
                } else {
                    let deny = Decision::Deny {
                        message: String::new(),
                        interrupt: false,
                    };
                    let result = answer_response(&ask, &deny);
                    self.stdin.send(&json!({"id": ask.id, "result": result}));
                }
            }
            Step::Resolved(id) => {
                let resolved = self
                    .asks
                    .iter()
                    .find(|(_, ask)| ask.id == id)
                    .map(|(&approval_id, _)| approval_id);
                if let Some(approval_id) = resolved {
                    self.asks.remove(&approval_id);
                    self.emit(Event::ApprovalWithdrawn { approval_id }).await;
                }
            }
            Step::Refuse { id, message } => {
                self.stdin
                    .send(&json!({"id": id, "error": refusal(&message)}));
            }
        }
    }

    /// Reads the response to one of plxd's requests.
    async fn reply(&mut self, id: u64, result: Result<Value, String>) {
        let Some(kind) = self.requests.remove(&id) else {
            return;
        };
        match (kind, result) {
            (Request::Initialize, Ok(_)) => {
                self.stdin.send(&json!({"method": "initialized"}));
                if let Some((method, params)) = self.thread.take() {
                    self.request(Request::Thread, method, &params);
                }
            }
            (Request::Thread, Ok(result)) => {
                let thread_id = result
                    .pointer("/thread/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                let Some(thread_id) = thread_id else {
                    self.fail_to_start("Codex started a thread without an id".into());
                    return;
                };
                let model = result
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.emit(Event::SessionStarted {
                    session_id: thread_id.clone(),
                    model,
                    api_key_source: None,
                })
                .await;
                self.thread_id = Some(thread_id);
                self.next_turn().await;
            }
            (Request::Initialize | Request::Thread, Err(message)) => self.fail_to_start(message),
            (Request::Turn, Ok(result)) => {
                if let Some(running) = &mut self.running {
                    running.codex_id = result
                        .pointer("/turn/id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                }
            }
            // The turn never ran.
            (Request::Turn, Err(message)) => {
                self.failure = Some(failure(super::stream::classify(&message), message));
                self.finish_running(None).await;
                self.next_turn().await;
            }
            (Request::Steer(turn), Ok(_)) => {
                self.emit(Event::TurnStarted {
                    turn_id: turn.turn_id,
                })
                .await;
                match (&mut self.running, turn.turn_id) {
                    (Some(running), Some(turn_id)) => running.steered.push(turn_id),
                    // The turn it joined has already completed.
                    (_, turn_id) => {
                        let result = None;
                        self.emit(Event::TurnFinished { turn_id, result }).await;
                    }
                }
            }
            // Codex wouldn't take it into the turn, which may have just ended: it goes next.
            (Request::Steer(turn), Err(message)) => {
                self.emit(Event::Notice {
                    detail: format!("Codex took the message as the next turn: {message}"),
                })
                .await;
                self.queued.push_front(turn);
                self.next_turn().await;
            }
        }
    }

    /// Ends the running turn, and the turns of the messages steered into it, with `result`.
    async fn finish_running(&mut self, result: Option<String>) {
        let Some(running) = self.running.take() else {
            let turn_id = None;
            self.emit(Event::TurnFinished { turn_id, result }).await;
            return;
        };
        let turns = std::iter::once(running.turn_id).chain(running.steered.into_iter().map(Some));
        for turn_id in turns {
            let result = result.clone();
            self.emit(Event::TurnFinished { turn_id, result }).await;
        }
    }

    /// The thread couldn't start, so no turn runs: the run fails and app-server exits.
    fn fail_to_start(&mut self, message: String) {
        self.failure = Some(failure(super::stream::classify(&message), message));
        self.stdin.close();
        self.control.close();
    }

    /// Starts the next queued turn, if the thread is ready and no turn is running.
    async fn next_turn(&mut self) {
        let Some(thread_id) = self.thread_id.clone() else {
            return;
        };
        if self.running.is_some() {
            return;
        }
        let Some(turn) = self.queued.pop_front() else {
            return;
        };
        // The prompt's TurnStarted went out when the run started.
        if std::mem::replace(&mut self.started_any, true) {
            self.emit(Event::TurnStarted {
                turn_id: turn.turn_id,
            })
            .await;
        }
        self.running = Some(Running {
            turn_id: turn.turn_id,
            codex_id: None,
            steered: Vec::new(),
        });
        let mut params = json!({"threadId": thread_id, "input": turn.input});
        if let Some(effort) = self.effort {
            params["effort"] = effort.into();
        }
        self.request(Request::Turn, "turn/start", &params);
    }

    async fn follow_up(&mut self, follow_up: FollowUp) {
        let paths = match write_images(&self.temp_dir, &follow_up.images) {
            Ok(Some((folder, paths))) => {
                self.images.push(folder);
                paths
            }
            Ok(None) => Vec::new(),
            Err(error) => {
                self.emit(Event::Warning {
                    warning: WarningKind::Other,
                    detail: format!("could not write a message's images: {error}"),
                })
                .await;
                Vec::new()
            }
        };
        let turn = Turn {
            turn_id: Some(follow_up.turn_id),
            input: input(&follow_up.text, &paths),
        };
        if !follow_up.steer {
            self.queued.push_back(turn);
        } else if let (Some(thread_id), Some(codex_id)) = (
            &self.thread_id,
            self.running
                .as_ref()
                .and_then(|running| running.codex_id.as_ref()),
        ) {
            let params =
                json!({"threadId": thread_id, "expectedTurnId": codex_id, "input": turn.input});
            self.request(Request::Steer(turn), "turn/steer", &params);
            return;
        } else {
            // No turn to steer into yet: it goes before anything else waiting, but after the
            // prompt.
            let at = usize::from(!self.started_any).min(self.queued.len());
            self.queued.insert(at, turn);
        }
        self.next_turn().await;
    }

    /// Writes `answer` to Codex, if it still waits on the request.
    fn answer(&mut self, answer: &Answer) {
        let Some(ask) = self.asks.remove(&answer.approval_id) else {
            return;
        };
        let result = answer_response(&ask, &answer.decision);
        self.stdin.send(&json!({"id": ask.id, "result": result}));
    }

    /// Closes stdin once no turn runs or waits, no steer or request waits on an answer, and plxd
    /// holds no message for Codex, so app-server exits.
    fn close_when_idle(&mut self) {
        let started = self.thread_id.is_some();
        let steering = self
            .requests
            .values()
            .any(|request| matches!(request, Request::Steer(_)));
        if started
            && self.running.is_none()
            && self.queued.is_empty()
            && self.asks.is_empty()
            && !steering
            && self.control.is_empty()
            && !self.held.now()
        {
            self.stdin.close();
            self.control.close();
        }
    }

    async fn emit(&mut self, event: Event) {
        if self.sink.emit(event).await.is_err() {
            self.switch.cancel();
        }
    }

    /// After app-server exited: reports every follow-up that never started a turn as dropped.
    async fn drop_undelivered(&mut self) {
        self.control.close();
        // The prompt, if it never ran, isn't a follow-up.
        if !self.started_any {
            self.queued.pop_front();
        }
        let mut dropped: Vec<TurnId> = self
            .queued
            .drain(..)
            .filter_map(|turn| turn.turn_id)
            .collect();
        for request in std::mem::take(&mut self.requests).into_values() {
            if let Request::Steer(Turn {
                turn_id: Some(turn_id),
                ..
            }) = request
            {
                dropped.push(turn_id);
            }
        }
        while let Ok(follow_up) = self.control.try_recv() {
            dropped.push(follow_up.turn_id);
        }
        for turn_id in dropped {
            self.emit(Event::FollowUpDropped { turn_id }).await;
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
        if exit.info.success() && self.turns_done > 0 && self.running.is_none() {
            return Outcome::Completed {
                result: self.last_result.take(),
            };
        }
        let failure = if super::stream::classify(&exit.stderr_tail) == FailureKind::NotSignedIn {
            failure(FailureKind::NotSignedIn, "Codex is not signed in".into())
        } else if exit.info.success() {
            failure(
                FailureKind::VendorError,
                "Codex exited without finishing its turn".into(),
            )
        } else {
            let message = match (exit.info.code, exit.info.signal) {
                (_, Some(signal)) => format!("Codex was killed by signal {signal}"),
                (Some(code), None) => format!("Codex exited with code {code}"),
                (None, None) => "Codex ended in an unknown way".to_owned(),
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
