//! Agent backends: one interface over the vendor CLIs that run agents (0004).
//!
//! A [`Backend`] starts a [`Run`] from a [`RunRequest`]. The run's CLI output arrives on an
//! [`EventStream`] as normalized [`Event`]s, so M3's runner and M4's coordinator never see vendor
//! differences. The adapters for Claude Code (#116), Codex (#122), and Cursor (#123) implement
//! these traits on top of [`process`], and [`fake`] implements them for tests.
//!
//! # Why trait objects, and no async methods
//!
//! Routing (#119) picks a backend per task at runtime and falls back to another, so backends live
//! in one registry as `Arc<dyn Backend>`. M3's runner keeps each run's control handle by `runId`
//! and calls [`Run::send`] and [`Run::cancel`] from whichever connection asks, while one task
//! drains the run's events; so the handle (`Arc<dyn Run>`) and the stream are separate values.
//!
//! Neither trait has an async method, which keeps both `dyn`-compatible without boxed futures.
//! [`Backend::start`] spawns the CLI and returns at once; everything after that, including a
//! failure, arrives on the stream; and `send` and `cancel` only enqueue. The price is a virtual
//! call per start, send, or cancel, never per event.

pub mod event;
pub mod fake;
pub mod process;

use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
pub use wisp_protocol::{RunId, TurnId};

pub use self::event::{
    CumulativeUsage, Event, ExitInfo, Failure, FailureKind, LimitStatus, LimitWindow, Outcome,
    ToolStatus, Usage, UsageDelta, WarningKind,
};
use self::process::SpawnError;

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
    /// started. Routing (#119) can fall back to another backend on any of these.
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
    /// Where the CLI runs: an absolute path to a directory, usually a worktree.
    pub cwd: PathBuf,
    /// The first message.
    pub prompt: String,
    /// What the agent's tools may do.
    pub policy: ToolPolicy,
    /// The account the run is charged to.
    pub account: AccountRef,
    /// The vendor's session to resume: an earlier run's [`Event::SessionStarted`] id.
    pub resume: Option<String>,
    /// The model, or the CLI's default.
    pub model: Option<String>,
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
    /// Edits inside the working directory: a worker's policy.
    WorkspaceWrite,
}

/// The account a run is charged to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountRef {
    /// wispd's id for the account (#114, #117).
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
    /// An API key from the Keychain (#117), injected into the CLI's environment at spawn only.
    ApiKey(ApiKey),
}

/// An API key. Its `Debug` hides it, and it doesn't serialize.
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

/// What a backend can do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent features, not states of one thing"
)]
pub struct Capabilities {
    /// [`Run::send`] works.
    pub follow_ups: bool,
    /// [`RunRequest::resume`] works.
    pub resume: bool,
    /// It can run the coordinator: no-write mode with wispd's MCP tools (0004: Claude Code and
    /// Codex, not Cursor).
    pub coordinator: bool,
    /// Its usage includes a cost.
    pub reports_cost: bool,
    /// Its runs report limit windows.
    pub rate_limits: bool,
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

/// What a [`RunHandle`] asks its backend's driver to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    /// Deliver a follow-up.
    FollowUp(FollowUp),
    /// Cancel the run.
    Cancel,
}

/// A [`Run`] that forwards to a driver task over a channel, which every backend can use.
///
/// It checks follow-ups for support and for idempotency, so drivers only see each turn id once.
/// The driver closing its receiver ends the run for [`Run::send`].
#[derive(Debug)]
pub struct RunHandle {
    id: RunId,
    follow_ups: bool,
    control: mpsc::UnboundedSender<Control>,
    turns: Mutex<HashMap<TurnId, String>>,
}

impl RunHandle {
    /// A handle for run `id`, and the receiver its driver reads.
    #[must_use]
    pub fn new(id: RunId, follow_ups: bool) -> (Self, mpsc::UnboundedReceiver<Control>) {
        let (control, receiver) = mpsc::unbounded_channel();
        let handle = Self {
            id,
            follow_ups,
            control,
            turns: Mutex::new(HashMap::new()),
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
            .send(Control::FollowUp(message))
            .map_err(|_| SendError::Finished)?;
        turns.insert(turn_id, text);
        Ok(())
    }

    fn cancel(&self) {
        let _ = self.control.send(Control::Cancel);
    }
}

/// The producing side of an [`EventStream`]. It enforces the stream's contract: exactly one
/// [`Event::Finished`], last.
#[derive(Debug)]
pub struct EventSink {
    events: mpsc::Sender<Event>,
    finished: bool,
}

/// The consumer dropped its [`EventStream`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the event stream was dropped")]
pub struct StreamClosed;

impl EventSink {
    /// A connected sink and stream that buffer `buffer` events.
    #[must_use]
    pub fn channel(buffer: usize) -> (Self, EventStream) {
        let (events, receiver) = mpsc::channel(buffer.max(1));
        let sink = Self {
            events,
            finished: false,
        };
        let stream = EventStream {
            events: receiver,
            usage: Usage::default(),
            outcome: None,
        };
        (sink, stream)
    }

    /// Sends `event`, waiting while the buffer is full. After an [`Event::Finished`], further
    /// events are dropped.
    ///
    /// # Errors
    ///
    /// [`StreamClosed`] once the consumer has dropped the stream. A backend then stops its CLI.
    pub async fn emit(&mut self, event: Event) -> Result<(), StreamClosed> {
        if self.finished {
            return Ok(());
        }
        self.finished = event.is_terminal();
        self.events.send(event).await.map_err(|_| StreamClosed)
    }

    /// Ends the stream with `outcome`.
    ///
    /// # Errors
    ///
    /// [`StreamClosed`] if the consumer has dropped the stream.
    pub async fn finish(&mut self, outcome: Outcome) -> Result<(), StreamClosed> {
        self.emit(Event::Finished { outcome }).await
    }

    /// Whether the stream has ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.finished
    }

    /// Completes once the consumer has dropped the stream. A backend then stops its CLI, since
    /// nothing would record what it does.
    pub async fn closed(&self) {
        self.events.closed().await;
    }
}

/// A run's events. It ends after exactly one [`Event::Finished`], and it sums the run's usage.
///
/// If the backend stops without finishing, for example because its task panicked, the stream
/// ends with a [`FailureKind::Internal`] failure, so a consumer always sees an outcome.
#[derive(Debug)]
pub struct EventStream {
    events: mpsc::Receiver<Event>,
    usage: Usage,
    outcome: Option<Outcome>,
}

impl EventStream {
    /// The next event, or `None` after [`Event::Finished`].
    pub async fn next(&mut self) -> Option<Event> {
        std::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }

    /// The sum of the run's [`Event::Usage`] deltas so far.
    #[must_use]
    pub fn usage(&self) -> Usage {
        self.usage
    }

    /// How the run ended, once [`Event::Finished`] has been read.
    #[must_use]
    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }
}

impl Stream for EventStream {
    type Item = Event;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Event>> {
        if self.outcome.is_some() {
            return Poll::Ready(None);
        }
        let event = match self.events.poll_recv(cx) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(Some(event)) => event,
            Poll::Ready(None) => Event::Finished {
                outcome: Outcome::Failed(Failure {
                    failure: FailureKind::Internal,
                    message: "the backend stopped without finishing the run".to_owned(),
                    exit: None,
                    stderr_tail: None,
                }),
            },
        };
        match &event {
            Event::Usage(delta) => self.usage += delta.usage,
            Event::Finished { outcome } => {
                self.outcome = Some(outcome.clone());
                self.events.close();
            }
            _ => {}
        }
        Poll::Ready(Some(event))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{
        Backend, Control, Event, EventSink, FailureKind, FollowUp, Outcome, Run, RunHandle, RunId,
        SendError, TurnId, Usage, UsageDelta,
    };

    #[test]
    fn the_traits_are_object_safe() {
        fn takes(_: Option<Arc<dyn Backend>>, _: Option<Arc<dyn Run>>) {}
        takes(None, None);
    }

    #[test]
    fn follow_ups_are_idempotent_on_their_turn_id() {
        let (handle, mut control) = RunHandle::new(RunId::generate(), true);
        let turn = FollowUp {
            turn_id: TurnId::generate(),
            text: "and the tests".into(),
        };
        handle.send(turn.clone()).unwrap();
        handle.send(turn.clone()).unwrap();
        assert_eq!(control.try_recv().unwrap(), Control::FollowUp(turn.clone()));
        assert!(control.try_recv().is_err(), "sent once");
        assert_eq!(
            handle.send(FollowUp {
                text: "something else".into(),
                ..turn
            }),
            Err(SendError::IdConflict)
        );
        drop(control);
        assert_eq!(
            handle.send(FollowUp {
                turn_id: TurnId::generate(),
                text: "late".into(),
            }),
            Err(SendError::Finished)
        );
    }

    #[test]
    fn a_backend_without_follow_ups_refuses_them() {
        let (handle, _control) = RunHandle::new(RunId::generate(), false);
        assert_eq!(
            handle.send(FollowUp {
                turn_id: TurnId::generate(),
                text: "hi".into()
            }),
            Err(SendError::Unsupported)
        );
    }

    #[tokio::test]
    async fn the_stream_ends_after_one_finished_and_sums_usage() {
        let (mut sink, mut stream) = EventSink::channel(8);
        let delta = |n| {
            Event::Usage(UsageDelta {
                model: None,
                usage: Usage {
                    input_tokens: n,
                    ..Usage::default()
                },
            })
        };
        sink.emit(delta(3)).await.unwrap();
        sink.emit(delta(4)).await.unwrap();
        sink.finish(Outcome::Cancelled).await.unwrap();
        sink.emit(delta(100)).await.unwrap();
        sink.finish(Outcome::Completed { result: None })
            .await
            .unwrap();
        assert!(sink.is_finished());
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        assert_eq!(events.len(), 3);
        assert_eq!(stream.outcome(), Some(&Outcome::Cancelled));
        assert_eq!(stream.usage().input_tokens, 7);
    }

    #[tokio::test]
    async fn a_backend_that_vanishes_still_ends_the_stream() {
        let (sink, mut stream) = EventSink::channel(8);
        drop(sink);
        let Some(Event::Finished {
            outcome: Outcome::Failed(failure),
        }) = stream.next().await
        else {
            panic!("expected a failure");
        };
        assert_eq!(failure.failure, FailureKind::Internal);
        assert_eq!(stream.next().await, None);
    }
}
