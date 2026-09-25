//! A fake backend for tests: a real child process that plays a scripted fixture.
//!
//! A [`Script`] is a list of [`Step`]s, usually read from a JSON fixture. The backend compiles it
//! into a `/bin/sh` program and runs that through [`Launcher`], so the fake goes through the same
//! supervision as the vendor adapters: spawning, the environment, line limits, stderr, cancel,
//! and reaping. The fake CLI's output format is [`Event`]'s JSON, one per line, plus
//! `{"kind": "usageTotal", ...}` lines for running totals.
//!
//! It is also the reference driver for the adapters: it reports the prompt's turn as soon as the
//! CLI starts, labels each `TurnFinished` with the oldest turn not yet finished, lets
//! [`CancelSwitch`] stop the process directly, and leaves usage totals to [`EventSink`].
//!
//! The child gets its arguments as the vendor CLIs would: the resume id, the prompt as a JSON
//! string, the policy, and the model. Follow-ups reach it on stdin, one JSON string per line.
//! With an API key account, the key is in `FAKE_API_KEY`; with a subscription it is scrubbed, as
//! 0004 has the Claude backend do with Anthropic's variables.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::net::unix::pipe;
use tokio::sync::mpsc;

use super::event::{Event, Failure, FailureKind, ModelUsage, Outcome, WarningKind};
use super::process::{
    CancelPolicy, Exit, Launcher, Output, OutputLimits, Process, ProcessSpec, StdinMode,
};
use super::{
    Backend, CancelSwitch, Capabilities, Credential, EVENT_BUFFER, EventSink, FollowUp, RunHandle,
    RunRequest, StartError, Started, ToolPolicy, TurnId,
};

/// The variable the fake CLI takes an API key from.
pub const API_KEY_ENV: &str = "FAKE_API_KEY";

/// The variable the fake CLI takes a second account's configuration folder from.
pub const CONFIG_HOME_ENV: &str = "FAKE_CONFIG_HOME";

/// A fake CLI's script.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(transparent)]
pub struct Script {
    /// The steps, in order. After the last one, the CLI exits with code 0.
    pub steps: Vec<Step>,
}

impl Script {
    /// Parses a JSON fixture: an array of steps.
    ///
    /// # Errors
    ///
    /// If the fixture is not a valid script.
    pub fn from_json(json: &str) -> serde_json::Result<Self> {
        serde_json::from_str(json)
    }
}

/// One step of a [`Script`]. In JSON, `"hang"` or `{"sleepMs": 10}`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Step {
    /// Prints [`Event::SessionStarted`], whose id is the resume id when the run resumes one, and
    /// `session_id` otherwise. Ids are limited to ASCII letters, digits, `.`, `_`, and `-`.
    Init {
        /// The session id of a new session.
        session_id: String,
        /// The model to report.
        #[serde(default)]
        model: Option<String>,
    },
    /// Prints an event.
    Emit(Event),
    /// Prints a running usage total for the session, as Codex and Claude report them. The
    /// backend turns it into an [`Event::Usage`] delta. In JSON, a [`ModelUsage`].
    UsageTotal(ModelUsage),
    /// Prints a line as it is.
    Raw(String),
    /// Prints a line of this many bytes.
    Oversized(usize),
    /// Writes a line to stderr.
    Stderr(String),
    /// Waits this many milliseconds.
    SleepMs(u64),
    /// Prints [`Event::Text`] with the working directory, unescaped.
    EchoCwd,
    /// Prints [`Event::Text`] with the prompt.
    EchoPrompt,
    /// Prints [`Event::Text`] with the tool policy.
    EchoPolicy,
    /// Prints [`Event::Text`] with the fake CLI's pid.
    EchoPid,
    /// Prints [`Event::Text`] with a variable's value, unescaped, or `<unset>`.
    EchoEnv(String),
    /// Waits for a follow-up on stdin and prints [`Event::Text`] with it. Exits with code 0 if
    /// stdin ends first.
    AwaitFollowUp,
    /// Prints [`Event::TurnFinished`]; the backend fills in the turn id.
    EndTurn {
        /// The turn's result.
        #[serde(default)]
        result: Option<String>,
    },
    /// Exits with this code.
    Exit(i32),
    /// Kills itself with `SIGKILL`.
    Crash,
    /// Ignores `SIGINT` from here on.
    IgnoreInterrupt,
    /// Waits forever.
    Hang,
}

/// A [`Backend`] that runs a [`Script`].
#[derive(Clone, Debug)]
pub struct FakeBackend {
    launcher: Launcher,
    script: Script,
    cancel: CancelPolicy,
    limits: OutputLimits,
    follow_ups: bool,
}

impl FakeBackend {
    /// A fake that plays `script` through `launcher`, taking follow-ups.
    #[must_use]
    pub fn new(launcher: Launcher, script: Script) -> Self {
        Self {
            launcher,
            script,
            cancel: CancelPolicy::default(),
            limits: OutputLimits::default(),
            follow_ups: true,
        }
    }

    /// Cancels with `policy` instead of the default.
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

    /// Takes no follow-ups, and gives the CLI a closed stdin, as `codex exec` needs.
    #[must_use]
    pub fn without_follow_ups(mut self) -> Self {
        self.follow_ups = false;
        self
    }
}

impl Backend for FakeBackend {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            follow_ups: self.follow_ups,
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
        if let Some(resume) = &request.resume
            && !is_safe_id(&resume.session_id)
        {
            return Err(StartError::Invalid(format!(
                "the resume id {:?} has characters the fake CLI doesn't take",
                resume.session_id
            )));
        }
        let script = compile(&self.script).map_err(StartError::Invalid)?;

        let mut spec = ProcessSpec::new("sh", &request.cwd);
        let policy = match request.policy {
            ToolPolicy::NoWrite => "no-write",
            ToolPolicy::WorkspaceWrite => "workspace-write",
        };
        let session = request
            .resume
            .as_ref()
            .map(|resume| resume.session_id.clone());
        spec.args = vec![
            "-c".into(),
            script.into(),
            "fake-cli".into(),
            session.unwrap_or_default().into(),
            json_string(&request.prompt).into(),
            policy.into(),
            request.model.clone().unwrap_or_default().into(),
        ];
        match &request.account.credential {
            Credential::Subscription { config_home } => {
                spec.scrub.push(API_KEY_ENV.into());
                if let Some(home) = config_home {
                    spec.inject.set(CONFIG_HOME_ENV, home);
                }
            }
            Credential::ApiKey(key) => {
                spec.inject.set(API_KEY_ENV, key.expose());
            }
        }
        spec.stdin = if self.follow_ups {
            StdinMode::Piped
        } else {
            StdinMode::Null
        };
        spec.limits = self.limits;

        let process = self.launcher.spawn(&spec)?;
        let switch = CancelSwitch::new();
        switch.arm(process.signals().clone(), self.cancel);
        let (handle, control) = RunHandle::new(request.run_id, self.follow_ups, switch.clone());
        let baseline = request
            .resume
            .map(|resume| resume.usage_totals)
            .unwrap_or_default();
        let (sink, events) = EventSink::channel(EVENT_BUFFER, baseline);
        tokio::spawn(drive(process, control, sink, switch, request.turn_id));
        Ok(Started {
            run: Arc::new(handle),
            events,
        })
    }
}

/// The result of writing one follow-up to the CLI's stdin.
enum Delivery {
    Written(TurnId),
    Failed(TurnId),
}

/// Writes follow-ups to stdin in order, off the driver's loop, so a CLI that stops reading stdin
/// can't keep the driver from reading its stdout.
async fn write_follow_ups(
    mut stdin: pipe::Sender,
    mut queue: mpsc::UnboundedReceiver<FollowUp>,
    results: mpsc::UnboundedSender<Delivery>,
) {
    let mut broken = false;
    while let Some(follow_up) = queue.recv().await {
        let mut line = json_string(&follow_up.text);
        line.push('\n');
        broken = broken || stdin.write_all(line.as_bytes()).await.is_err();
        let result = if broken {
            Delivery::Failed(follow_up.turn_id)
        } else {
            Delivery::Written(follow_up.turn_id)
        };
        let _ = results.send(result);
    }
}

/// The follow-ups' way to the CLI: a queue into [`write_follow_ups`], and its results.
struct Stdin {
    queue: Option<mpsc::UnboundedSender<FollowUp>>,
    results: Option<mpsc::UnboundedReceiver<Delivery>>,
    writer: Option<tokio::task::JoinHandle<()>>,
}

impl Stdin {
    /// Starts the writer, or closes `control` when the CLI's stdin isn't a pipe.
    fn start(process: &mut Process, control: &mut mpsc::UnboundedReceiver<FollowUp>) -> Self {
        let Some(stdin) = process.take_stdin() else {
            control.close();
            return Self {
                queue: None,
                results: None,
                writer: None,
            };
        };
        let (queue, queue_rx) = mpsc::unbounded_channel();
        let (results, results_rx) = mpsc::unbounded_channel();
        Self {
            queue: Some(queue),
            results: Some(results_rx),
            writer: Some(tokio::spawn(write_follow_ups(stdin, queue_rx, results))),
        }
    }

    /// After the CLI exited: reports every follow-up that was queued, or written but not yet
    /// reported, as dropped. The writer fails what is still queued against the closed pipe.
    async fn drop_undelivered(
        mut self,
        control: &mut mpsc::UnboundedReceiver<FollowUp>,
        sink: &mut EventSink,
    ) {
        control.close();
        while let Ok(follow_up) = control.try_recv() {
            if let Some(queue) = &self.queue {
                let _ = queue.send(follow_up);
            }
        }
        drop(self.queue.take());
        if let Some(writer) = self.writer.take() {
            // Writes fail at once once nothing holds the pipe's read end. Something the CLI
            // started outside its process group could, so don't wait on it for long.
            let _ = tokio::time::timeout(Duration::from_secs(1), writer).await;
        }
        if let Some(results) = &mut self.results {
            while let Ok(delivery) = results.try_recv() {
                let turn_id = match delivery {
                    Delivery::Written(turn_id) | Delivery::Failed(turn_id) => turn_id,
                };
                let _ = sink.emit(Event::FollowUpDropped { turn_id }).await;
            }
        }
    }
}

/// Runs one fake run: forwards the CLI's events, delivers follow-ups, and decides the outcome
/// when the CLI exits. Cancelling doesn't go through here: the run's handle signals the process
/// through `switch`, so it works even while this task waits for a consumer that isn't reading.
async fn drive(
    mut process: Process,
    mut control: mpsc::UnboundedReceiver<FollowUp>,
    mut sink: EventSink,
    switch: CancelSwitch,
    first_turn: Option<TurnId>,
) {
    let mut stdin = Stdin::start(&mut process, &mut control);
    let mut state = State {
        switch,
        turns: VecDeque::from([first_turn]),
        reported: None,
        last_result: None,
    };
    let mut control_open = true;
    let first = Event::TurnStarted {
        turn_id: first_turn,
    };
    if sink.emit(first).await.is_err() {
        state.switch.cancel();
    }

    let exit = loop {
        tokio::select! {
            // Deliveries first, so a follow-up's TurnStarted comes before what the CLI answers.
            biased;
            Some(delivery) = recv(&mut stdin.results) => {
                let event = match delivery {
                    Delivery::Written(turn_id) => {
                        state.turns.push_back(Some(turn_id));
                        Event::TurnStarted { turn_id: Some(turn_id) }
                    }
                    Delivery::Failed(turn_id) => {
                        // stdin is gone, so no later follow-up can arrive either.
                        control.close();
                        Event::FollowUpDropped { turn_id }
                    }
                };
                if sink.emit(event).await.is_err() {
                    state.switch.cancel();
                }
            }
            output = process.next() => {
                let sent = match output {
                    Some(Output::Line(line)) => match state.parse(&line) {
                        Parsed::Event(Some(event)) => sink.emit(event).await,
                        Parsed::Event(None) => Ok(()),
                        Parsed::Total(total) => {
                            sink.observe_total(total.model.as_deref(), total.usage).await
                        }
                    },
                    Some(Output::Oversized { bytes }) => {
                        sink.emit(Event::Warning {
                            warning: WarningKind::OversizedLine,
                            detail: format!("skipped a {bytes}-byte line"),
                        })
                        .await
                    }
                    Some(Output::Exited(exit)) => break Some(exit),
                    None => break None,
                };
                if sent.is_err() {
                    state.switch.cancel();
                }
            }
            follow_up = control.recv(), if control_open => match follow_up {
                Some(follow_up) => match &stdin.queue {
                    Some(queue) if queue.send(follow_up.clone()).is_ok() => {}
                    _ => {
                        let dropped = Event::FollowUpDropped { turn_id: follow_up.turn_id };
                        let _ = sink.emit(dropped).await;
                    }
                },
                None => control_open = false,
            },
            () = sink.closed(), if !state.switch.is_cancelled() => state.switch.cancel(),
        }
    };

    stdin.drop_undelivered(&mut control, &mut sink).await;
    let outcome = state.outcome(exit);
    let _ = sink.finish(outcome).await;
}

async fn recv<T>(receiver: &mut Option<mpsc::UnboundedReceiver<T>>) -> Option<T> {
    match receiver {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// A line of the fake CLI's output.
enum Parsed {
    /// An event to forward, or nothing.
    Event(Option<Event>),
    /// A running usage total.
    Total(ModelUsage),
}

struct State {
    switch: CancelSwitch,
    /// Turns the CLI has taken but not finished, oldest first.
    turns: VecDeque<Option<TurnId>>,
    /// The outcome the CLI reported itself, before any cancel.
    reported: Option<Outcome>,
    last_result: Option<String>,
}

impl State {
    fn parse(&mut self, line: &[u8]) -> Parsed {
        let warning =
            |warning, detail: String| Parsed::Event(Some(Event::Warning { warning, detail }));
        let value = match serde_json::from_slice::<serde_json::Value>(line) {
            Ok(value) => value,
            Err(error) => return warning(WarningKind::MalformedLine, error.to_string()),
        };
        let kind = value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned);
        if kind.as_deref() == Some("usageTotal") {
            return match serde_json::from_value::<ModelUsage>(value) {
                Ok(total) => Parsed::Total(total),
                Err(error) => warning(WarningKind::MalformedLine, error.to_string()),
            };
        }
        let event = match serde_json::from_value::<Event>(value) {
            Ok(event) => event,
            Err(error) => return warning(WarningKind::MalformedLine, error.to_string()),
        };
        let event = match event {
            Event::Unknown => {
                return warning(
                    WarningKind::UnknownEvent,
                    format!("an event of kind {:?}", kind.unwrap_or_default()),
                );
            }
            Event::TurnFinished { result, .. } => {
                self.last_result.clone_from(&result);
                Event::TurnFinished {
                    turn_id: self.turns.pop_front().flatten(),
                    result,
                }
            }
            Event::Finished { outcome, .. } => {
                if !self.switch.is_cancelled() && self.reported.is_none() {
                    self.reported = Some(outcome);
                }
                return Parsed::Event(None);
            }
            event => event,
        };
        Parsed::Event(Some(event))
    }

    fn outcome(self, exit: Option<Exit>) -> Outcome {
        if let Some(reported) = self.reported {
            return reported;
        }
        if self.switch.is_cancelled() {
            return Outcome::Cancelled;
        }
        let Some(exit) = exit else {
            return Outcome::Failed(Failure {
                failure: FailureKind::Internal,
                message: "lost track of the process".into(),
                exit: None,
                stderr_tail: None,
            });
        };
        if exit.info.success() {
            return Outcome::Completed {
                result: self.last_result,
            };
        }
        let message = match (exit.info.code, exit.info.signal) {
            (_, Some(signal)) => format!("the CLI was killed by signal {signal}"),
            (Some(code), None) => format!("the CLI exited with code {code}"),
            (None, None) => "the CLI ended in an unknown way".to_owned(),
        };
        Outcome::Failed(Failure {
            failure: FailureKind::Crashed,
            message,
            exit: Some(exit.info),
            stderr_tail: (!exit.stderr_tail.is_empty()).then_some(exit.stderr_tail),
        })
    }
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn json_string(text: &str) -> String {
    serde_json::Value::from(text).to_string()
}

/// Quotes `text` for the shell.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// `printf` for a line of JSON made of literal pieces and shell words, which the shell expands.
fn print_json(out: &mut String, pieces: &[Piece<'_>]) {
    for piece in pieces {
        match piece {
            Piece::Literal(text) => write!(out, "printf '%s' {}; ", quote(text)),
            Piece::Word(word) => write!(out, "printf '%s' {word}; "),
        }
        .expect("writing to a String never fails");
    }
    out.push_str("echo\n");
}

enum Piece<'a> {
    Literal(&'a str),
    Word(&'a str),
}

fn print_text(out: &mut String, word: &str, quoted: bool) {
    if quoted {
        print_json(
            out,
            &[
                Piece::Literal(r#"{"kind":"text","text":"#),
                Piece::Word(word),
                Piece::Literal("}"),
            ],
        );
    } else {
        print_json(
            out,
            &[
                Piece::Literal(r#"{"kind":"text","text":""#),
                Piece::Word(word),
                Piece::Literal(r#""}"#),
            ],
        );
    }
}

/// Compiles a script into a `/bin/sh` program. It takes `$1` resume id, `$2` prompt as a JSON
/// string, `$3` policy, and `$4` model.
fn compile(script: &Script) -> Result<String, String> {
    let mut out = String::from("resume=$1; prompt=$2; policy=$3\n");
    for step in &script.steps {
        match step {
            Step::Init { session_id, model } => {
                if !is_safe_id(session_id) {
                    return Err(format!("unsafe session id {session_id:?} in the script"));
                }
                writeln!(out, "session=${{resume:-{session_id}}}").expect("infallible");
                let mut rest = String::from("\"");
                if let Some(model) = model {
                    write!(rest, ",\"model\":{}", json_string(model)).expect("infallible");
                }
                rest.push('}');
                print_json(
                    &mut out,
                    &[
                        Piece::Literal(r#"{"kind":"sessionStarted","sessionId":""#),
                        Piece::Word("\"$session\""),
                        Piece::Literal(&rest),
                    ],
                );
            }
            Step::Emit(event) => {
                let json = serde_json::to_string(event).map_err(|e| e.to_string())?;
                writeln!(out, "printf '%s\\n' {}", quote(&json)).expect("infallible");
            }
            Step::UsageTotal(total) => {
                let mut json = serde_json::to_value(total).map_err(|e| e.to_string())?;
                json["kind"] = "usageTotal".into();
                writeln!(out, "printf '%s\\n' {}", quote(&json.to_string())).expect("infallible");
            }
            Step::Raw(line) => {
                writeln!(out, "printf '%s\\n' {}", quote(line)).expect("infallible");
            }
            Step::Oversized(bytes) => {
                writeln!(out, "head -c {bytes} /dev/zero | tr '\\0' x; echo").expect("infallible");
            }
            Step::Stderr(line) => {
                writeln!(out, "printf '%s\\n' {} >&2", quote(line)).expect("infallible");
            }
            Step::SleepMs(ms) => {
                writeln!(out, "sleep {}.{:03} & wait $!", ms / 1000, ms % 1000)
                    .expect("infallible");
            }
            Step::EchoCwd => print_text(&mut out, "\"$PWD\"", false),
            Step::EchoPrompt => print_text(&mut out, "\"$prompt\"", true),
            Step::EchoPolicy => print_text(&mut out, "\"$policy\"", false),
            Step::EchoPid => print_text(&mut out, "\"$$\"", false),
            Step::EchoEnv(name) => {
                if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                {
                    return Err(format!("unsafe variable name {name:?} in the script"));
                }
                print_text(&mut out, &format!("\"${{{name}-<unset>}}\""), false);
            }
            Step::AwaitFollowUp => {
                out.push_str("IFS= read -r line || exit 0\n");
                print_text(&mut out, "\"$line\"", true);
            }
            Step::EndTurn { result } => {
                let event = Event::TurnFinished {
                    turn_id: None,
                    result: result.clone(),
                };
                let json = serde_json::to_string(&event).map_err(|e| e.to_string())?;
                writeln!(out, "printf '%s\\n' {}", quote(&json)).expect("infallible");
            }
            Step::Exit(code) => writeln!(out, "exit {code}").expect("infallible"),
            Step::Crash => out.push_str("kill -KILL $$\n"),
            Step::IgnoreInterrupt => out.push_str("trap '' INT\n"),
            Step::Hang => out.push_str("while :; do sleep 60 & wait $!; done\n"),
        }
    }
    out.push_str("exit 0\n");
    Ok(out)
}

#[cfg(test)]
mod tests;
