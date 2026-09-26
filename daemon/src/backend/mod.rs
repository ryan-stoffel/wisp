//! Agent backends: one interface over the vendor CLIs that run agents (0004).
//!
//! A [`Backend`] starts a [`Run`] from a [`RunRequest`]; the CLI's output arrives on an
//! [`EventStream`] as normalized [`Event`]s. The handle and the stream are separate values, so
//! one task drains events while any connection can send or cancel.
//!
//! Neither trait has an async method, which keeps both `dyn`-compatible without boxed futures:
//! [`Backend::start`] spawns the CLI and returns at once, everything after that arrives on the
//! stream, and `send` and `cancel` only enqueue.

pub mod claude;
pub mod event;
pub mod fake;
pub mod key_account;
pub mod process;
pub mod sandbox;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
pub use wisp_protocol::{RunId, TurnId};
use zeroize::Zeroize;

pub use self::event::{
    CumulativeUsage, Event, ExitInfo, Failure, FailureKind, LimitStatus, LimitWindow, ModelUsage,
    Outcome, TodoItem, TodoStatus, ToolStatus, Usage, WarningKind,
};
use self::process::{CancelPolicy, Signals, SpawnError};
pub use self::sandbox::WorkerSandbox;

/// How many events a run buffers before its backend waits for the consumer.
pub const EVENT_BUFFER: usize = 256;

/// A vendor CLI that runs agents.
pub trait Backend: Send + Sync {
    /// A short, stable name for logs and records, such as `claude`, `codex`, or `cursor`.
    fn name(&self) -> &'static str;

    /// What this backend can do.
    fn capabilities(&self) -> Capabilities;

    /// Starts a run. Returns once the CLI has been spawned; the rest arrives on the stream.
    ///
    /// Must be called inside a tokio runtime. Starting the same `run_id` twice starts two
    /// processes: making `agent/start` idempotent (0007) is the caller's job.
    ///
    /// # Errors
    ///
    /// If the request is invalid or asks for something the backend can't do, or the CLI can't be
    /// started.
    fn start(&self, request: RunRequest) -> Result<Started, StartError>;
}

/// A started run: its control handle and its events.
pub struct Started {
    /// Sends follow-ups and cancels. Clone the `Arc` to control the run from several places.
    pub run: Arc<dyn Run>,
    /// The run's events, ending with exactly one [`Event::Finished`]. Dropping it cancels the
    /// run.
    pub events: EventStream,
}

impl fmt::Debug for Started {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Started")
            .field("run", &self.run.id())
            .finish_non_exhaustive()
    }
}

/// A run's control handle.
pub trait Run: Send + Sync {
    /// The id the caller gave the run.
    fn id(&self) -> RunId;

    /// Sends a follow-up message into the run (0011's `agent/send`), as the next turn.
    ///
    /// Idempotent on the message's `turn_id`: sending the same id and text again succeeds without
    /// sending it twice. The run reports [`Event::TurnStarted`] once the CLI has the message, or
    /// [`Event::FollowUpDropped`] if the run ended first.
    ///
    /// # Errors
    ///
    /// [`SendError::Unsupported`] if the backend takes no follow-ups, [`SendError::Finished`] once
    /// the run has ended (resume its session in a new run instead), and
    /// [`SendError::IdConflict`] if the id was already used with another text.
    fn send(&self, message: FollowUp) -> Result<(), SendError>;

    /// Stops the run: the backend's graceful signal, then `SIGKILL` to the CLI's process group if
    /// it hasn't exited after a grace period. Returns at once, and calling it again does nothing.
    /// The run ends with [`Outcome::Cancelled`], unless it had already finished.
    fn cancel(&self);
}

/// A task for a backend.
#[derive(Clone, Debug)]
pub struct RunRequest {
    /// The caller's id for the run (0007's `agent/start {runId}`).
    pub run_id: RunId,
    /// The caller's id for the prompt's turn, which the run's first [`Event::TurnStarted`] and
    /// [`Event::TurnFinished`] carry.
    pub turn_id: Option<TurnId>,
    /// Where the CLI runs: an absolute path to a directory, usually a worktree.
    pub cwd: PathBuf,
    /// The first message.
    pub prompt: String,
    /// What the agent's tools may do.
    pub policy: ToolPolicy,
    /// Where a [`ToolPolicy::WorkspaceWrite`] run may write and what it may not read (0013).
    /// Required for a worker, which is refused without one; a no-write run ignores it.
    pub sandbox: Option<WorkerSandbox>,
    /// The account the run is charged to.
    pub account: AccountRef,
    /// The vendor's session to resume, or a new session.
    pub resume: Option<Resume>,
    /// The model, or the CLI's default.
    pub model: Option<String>,
}

/// A vendor session for a run to continue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resume {
    /// The session's id: an earlier run's [`Event::SessionStarted`] id.
    pub session_id: String,
    /// The session's running usage totals per model: the `usage_totals` of the last run of it
    /// that finished, so the new run reports only what it adds even though vendors carry totals
    /// over into a resumed session.
    pub usage_totals: Vec<ModelUsage>,
}

/// A follow-up message for a running agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowUp {
    /// The caller's id for the turn (0011's `agent/send {turnId}`).
    pub turn_id: TurnId,
    /// The message.
    pub text: String,
}

/// What an agent's tools may do (0004's policies).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolPolicy {
    /// Read-only tools, no hooks, no project settings: the coordinator's policy.
    NoWrite,
    /// Edits inside the working directory, and commands in the vendor's OS sandbox: a worker's
    /// policy, bounded by the run's [`WorkerSandbox`] (0013). Backends never commit; the runner
    /// does, after [`Event::Finished`].
    WorkspaceWrite,
}

/// The account a run is charged to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRef {
    /// wispd's id for the account.
    pub id: String,
    /// How the CLI authenticates.
    pub credential: Credential,
}

/// How a CLI authenticates for a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Credential {
    /// The login the user made in the vendor's own CLI. The backend scrubs every credential
    /// variable that would outrank it (0004).
    Subscription {
        /// The CLI's configuration folder for a second account on this machine, such as
        /// `CLAUDE_CONFIG_DIR` or `CODEX_HOME`, or the CLI's default.
        config_home: Option<PathBuf>,
    },
    /// An API key from the Keychain, injected into the CLI's environment at spawn only.
    ApiKey(ApiKey),
}

/// An API key. Its `Debug` hides it, it doesn't serialize, and it zeroizes its buffer on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a key.
    #[must_use]
    pub fn new(key: String) -> Self {
        Self(key)
    }

    /// The key itself, for the one place that injects it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

impl Drop for ApiKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// What a backend can do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Capabilities {
    /// It enforces the worker sandbox (0013) for a [`ToolPolicy::WorkspaceWrite`] run, so the
    /// runner may start workers on it.
    pub worker_sandbox: bool,
}

/// Why a run could not start.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The request is malformed.
    #[error("invalid run request: {0}")]
    Invalid(String),
    /// The request asks for something this backend doesn't do.
    #[error("{0}")]
    Unsupported(String),
    /// The CLI could not be started.
    #[error(transparent)]
    Spawn(#[from] SpawnError),
}

/// Why [`Run::send`] refused a message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SendError {
    /// The backend takes no follow-ups.
    #[error("this backend takes no follow-up messages")]
    Unsupported,
    /// The run has ended.
    #[error("the run has finished")]
    Finished,
    /// The turn id was already used with another text.
    #[error("the turn id was already used for a different message")]
    IdConflict,
}

/// Cancels a run's current process, from any thread, without waiting on the run's driver.
///
/// [`Run::cancel`] has to work while the driver is stuck waiting for a consumer that isn't
/// reading, so the handle signals the process itself. The driver arms the switch with each
/// process it starts, and asks [`CancelSwitch::is_cancelled`] when it decides the outcome.
#[derive(Clone, Debug, Default)]
pub struct CancelSwitch {
    state: Arc<Mutex<SwitchState>>,
}

#[derive(Debug, Default)]
struct SwitchState {
    cancelled: bool,
    target: Option<(Signals, CancelPolicy)>,
}

impl CancelSwitch {
    /// A switch that hasn't been flipped and has no process yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes `signals`' process the one a cancel stops, with `policy`. If the run was already
    /// cancelled, it stops that process at once.
    pub fn arm(&self, signals: Signals, policy: CancelPolicy) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.cancelled {
            signals.cancel(policy);
        }
        state.target = Some((signals, policy));
    }

    /// Cancels the run: stops the armed process, if any, with its policy. Only the first call
    /// does anything.
    pub fn cancel(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.cancelled {
            return;
        }
        state.cancelled = true;
        if let Some((signals, policy)) = &state.target {
            signals.cancel(*policy);
        }
    }

    /// Whether the run was cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cancelled
    }
}

/// A [`Run`] that every backend can use: it sends follow-ups to the backend's driver over a
/// channel, and cancels through a [`CancelSwitch`] the driver arms.
///
/// It checks follow-ups for support and for idempotency, so drivers only see each turn id once.
/// The driver closes its receiver once it can deliver no more follow-ups; from then on every
/// [`Run::send`], including a retry of a follow-up that was accepted but dropped, fails with
/// [`SendError::Finished`].
#[derive(Debug)]
pub struct RunHandle {
    id: RunId,
    follow_ups: bool,
    control: mpsc::UnboundedSender<FollowUp>,
    turns: Mutex<HashMap<TurnId, String>>,
    cancel: CancelSwitch,
}

impl RunHandle {
    /// A handle for run `id` that cancels through `cancel`, and the receiver of follow-ups its
    /// driver reads.
    #[must_use]
    pub fn new(
        id: RunId,
        follow_ups: bool,
        cancel: CancelSwitch,
    ) -> (Self, mpsc::UnboundedReceiver<FollowUp>) {
        let (control, receiver) = mpsc::unbounded_channel();
        let handle = Self {
            id,
            follow_ups,
            control,
            turns: Mutex::new(HashMap::new()),
            cancel,
        };
        (handle, receiver)
    }
}

impl Run for RunHandle {
    fn id(&self) -> RunId {
        self.id
    }

    fn send(&self, message: FollowUp) -> Result<(), SendError> {
        if !self.follow_ups {
            return Err(SendError::Unsupported);
        }
        if self.control.is_closed() {
            return Err(SendError::Finished);
        }
        let mut turns = self.turns.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(text) = turns.get(&message.turn_id) {
            return if *text == message.text {
                Ok(())
            } else {
                Err(SendError::IdConflict)
            };
        }
        let (turn_id, text) = (message.turn_id, message.text.clone());
        self.control
            .send(message)
            .map_err(|_| SendError::Finished)?;
        turns.insert(turn_id, text);
        Ok(())
    }

    fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// The producing side of an [`EventStream`]. It enforces the stream's contract, exactly one
/// [`Event::Finished`] and last, and it keeps the session's usage totals for that event.
#[derive(Debug)]
pub struct EventSink {
    events: mpsc::Sender<Event>,
    finished: bool,
    usage: CumulativeUsage,
}

/// The consumer dropped its [`EventStream`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the event stream was dropped")]
pub struct StreamClosed;

impl EventSink {
    /// A connected sink and stream that buffer `buffer` events, for a session whose usage so far
    /// is `usage_totals`: a resumed session's [`Resume::usage_totals`], or nothing.
    #[must_use]
    pub fn channel(buffer: usize, usage_totals: Vec<ModelUsage>) -> (Self, EventStream) {
        let (events, receiver) = mpsc::channel(buffer.max(1));
        let sink = Self {
            events,
            finished: false,
            usage: CumulativeUsage::with_baseline(usage_totals),
        };
        let stream = EventStream {
            events: receiver,
            done: false,
        };
        (sink, stream)
    }

    /// Sends `event`, waiting while the buffer is full. After an [`Event::Finished`], further
    /// events are dropped.
    ///
    /// An [`Event::Usage`] is a delta and adds to the session's totals. An [`Event::Finished`]
    /// gets the totals filled in, whatever it carried.
    ///
    /// # Errors
    ///
    /// [`StreamClosed`] once the consumer has dropped the stream. A backend then stops its CLI.
    pub async fn emit(&mut self, mut event: Event) -> Result<(), StreamClosed> {
        match &mut event {
            Event::Usage(delta) => self.usage.record(delta),
            Event::Finished { usage_totals, .. } => *usage_totals = self.usage.totals(),
            _ => {}
        }
        self.send(event).await
    }

    /// Takes a running total that the vendor reported for `model`, such as Codex's
    /// `turn.completed.usage`, and sends the [`Event::Usage`] delta since the last one, if any.
    ///
    /// # Errors
    ///
    /// [`StreamClosed`] once the consumer has dropped the stream.
    pub async fn observe_total(
        &mut self,
        model: Option<&str>,
        total: Usage,
    ) -> Result<(), StreamClosed> {
        match self.usage.observe(model, total) {
            Some(delta) => self.send(Event::Usage(delta)).await,
            None => Ok(()),
        }
    }

    /// Ends the stream with `outcome` and the session's usage totals.
    ///
    /// # Errors
    ///
    /// [`StreamClosed`] if the consumer has dropped the stream.
    pub async fn finish(&mut self, outcome: Outcome) -> Result<(), StreamClosed> {
        self.emit(Event::Finished {
            outcome,
            usage_totals: Vec::new(),
        })
        .await
    }

    /// Discards the running usage total this sink has summed so far and starts over from
    /// `baseline`: for routing's fallback, whose `Finished` must report only the fallback
    /// session's own totals.
    pub fn reset_usage(&mut self, baseline: Vec<ModelUsage>) {
        self.usage = CumulativeUsage::with_baseline(baseline);
    }

    /// Completes once the consumer has dropped the stream. A backend then stops its CLI, since
    /// nothing would record what it does.
    pub async fn closed(&self) {
        self.events.closed().await;
    }

    async fn send(&mut self, event: Event) -> Result<(), StreamClosed> {
        if self.finished {
            return Ok(());
        }
        self.finished = event.is_terminal();
        self.events.send(event).await.map_err(|_| StreamClosed)
    }
}

/// A run's events. It ends after exactly one [`Event::Finished`].
///
/// If the backend stops without finishing, for example because its task panicked, the stream
/// ends with a [`FailureKind::Internal`] failure, so a consumer always sees an outcome. That
/// failure has no usage totals; keep the session's previous ones.
#[derive(Debug)]
pub struct EventStream {
    events: mpsc::Receiver<Event>,
    done: bool,
}

impl EventStream {
    /// The next event, or `None` after [`Event::Finished`]. Cancel-safe.
    pub async fn next(&mut self) -> Option<Event> {
        if self.done {
            return None;
        }
        let event = self.events.recv().await.unwrap_or_else(|| Event::Finished {
            outcome: Outcome::Failed(Failure {
                failure: FailureKind::Internal,
                message: "the backend stopped without finishing the run".to_owned(),
                exit: None,
                stderr_tail: None,
            }),
            usage_totals: Vec::new(),
        });
        if event.is_terminal() {
            self.done = true;
            self.events.close();
        }
        Some(event)
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, EventSink, EventStream, FailureKind, ModelUsage, Outcome, Usage};

    fn input(n: u64) -> Event {
        Event::Usage(ModelUsage {
            model: None,
            usage: Usage {
                input_tokens: n,
                ..Usage::default()
            },
        })
    }

    async fn all(mut stream: EventStream) -> Vec<Event> {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn the_stream_ends_after_one_finished_and_sums_usage() {
        let (mut sink, stream) = EventSink::channel(8, Vec::new());
        sink.emit(input(3)).await.unwrap();
        sink.emit(input(4)).await.unwrap();
        sink.finish(Outcome::Cancelled).await.unwrap();
        sink.emit(input(100)).await.unwrap();
        sink.finish(Outcome::Completed { result: None })
            .await
            .unwrap();
        let events = all(stream).await;
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[2],
            Event::Finished {
                outcome: Outcome::Cancelled,
                usage_totals: vec![ModelUsage {
                    model: None,
                    usage: Usage {
                        input_tokens: 7,
                        ..Usage::default()
                    },
                }],
            }
        );
    }

    #[tokio::test]
    async fn resetting_usage_drops_what_was_summed_before_it() {
        let (mut sink, stream) = EventSink::channel(8, Vec::new());
        sink.emit(input(100)).await.unwrap();
        sink.reset_usage(Vec::new());
        sink.emit(input(4)).await.unwrap();
        sink.finish(Outcome::Completed { result: None })
            .await
            .unwrap();
        let Some(Event::Finished { usage_totals, .. }) = all(stream).await.pop() else {
            panic!("no Finished");
        };
        assert_eq!(
            usage_totals[0].usage.input_tokens, 4,
            "the reset total, not 104"
        );
    }

    #[tokio::test]
    async fn a_backend_that_vanishes_still_ends_the_stream() {
        let (sink, mut stream) = EventSink::channel(8, Vec::new());
        drop(sink);
        let Some(Event::Finished {
            outcome: Outcome::Failed(failure),
            ..
        }) = stream.next().await
        else {
            panic!("expected a failure");
        };
        assert_eq!(failure.failure, FailureKind::Internal);
        assert_eq!(stream.next().await, None);
    }
}
