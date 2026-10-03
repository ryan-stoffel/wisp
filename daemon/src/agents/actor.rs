//! One run's actor: the task that owns a run for as long as plxd runs.
//!
//! It takes commands (`agent/send`, `agent/cancel`, `agent/approve`, `agent/accept`,
//! `agent/openPr`, the Git menu's in `git`, `thread/delete`) and the run's backend events in one
//! loop, so nothing about a run needs a lock, and events are logged in the order they happened.
//!
//! It also keeps the permission requests its CLI waits on (RYA-222, decision 0031): it logs each
//! one with when it expires, passes `agent/approve`'s answer to the CLI, denies one nobody
//! answered in time, and logs how each one ended, including when a cancel, a stop, or the CLI's
//! exit ends it first.
//!
//! A message sent while its CLI works on a turn waits in the run's queue (PLX-370, decision
//! 0048), stored so a restart keeps it, until the turn ends; then it goes to the same CLI, which
//! [`Run::hold`] keeps open for it. A message that changes what its CLI runs with (its model,
//! another run option, or account) can't reach a CLI that's running, so it waits, with every
//! message after it, until that CLI exits; then each goes to a new CLI process in turn. A new
//! account on another backend moves the run there: the session can't follow, so a new one starts
//! in the same place, told the conversation so far. Clients list, edit, reorder, and cancel what
//! waits, and a steer goes into the running turn instead, through the backend, or by cancelling
//! the CLI and resuming it with the message where the backend takes no messages while it runs.
//!
//! A project's coordinator (0024) differs in four places: it starts in a detached worktree of
//! the project's repository (RYA-171) with plxd's tools and no sandbox, that worktree is checked
//! after every turn (0004), it is never committed, and runs it started wake it when they finish
//! (RYA-42, [`super::wake`]).
//!
//! A thread in its repository's own checkout has no worktree: every launch, a resume included,
//! starts in the checkout, and it is never committed either.
//!
//! A run a usage limit stopped waits for the limit to reset and resumes itself (PLX-371,
//! [`waiting`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::{AcceptId, AgentMerge};
use parallax_protocol::{
    AccountChoice, AccountId, AgentApprovalAnswer, AgentApprovalBy, AgentApprovalDecision,
    AgentApproveParams, AgentApproveResult, AgentFailureKind, AgentOutcome, AgentOutputItem,
    AgentRun, ApprovalId, CoordinatorThreadId, DiffSummary, ErrorKind, GitStatus, ImageId,
    ParallaxEvent, ProjectId, PromptImage, QueueResult, QueuedMessage, Role, RunId, TurnId,
};
use parallax_store::{
    QueuedRow, Run as RunRow, RunAccept, SessionModelUsage, StoredImage, Worktree,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::approvals::{self, Approvals, Lookup, ended};
use super::attached;
use super::convert::{
    self, WORKSPACE_WRITE, agent_run, item_bytes, option_name, option_value, output_item,
};
use super::resume::Resumes;
use super::wake::{self, Wakes};
use super::worker::{sandbox_path, worker_prompt, worker_unavailable};
use super::{Place, Prepared, RunOptions, prepare, store, store_error};
use crate::backend::{
    AccountRef, Answer, AnswerError, Backend, CoordinatorTools, Credential, Decision, Event,
    EventStream, FollowUp, ModelUsage, Outcome, Resume, Run, RunRequest, SendError, Usage,
    WorkerSandbox,
    run_temp::{self, RunTemp},
};
use crate::routing;
use crate::server::Daemon;
use crate::worktree::github_pr_urls;

mod git;
mod waiting;
pub(crate) use git::GitAction;

/// How long transcript items wait to be sent together as one `agent.output` (0007).
const COALESCE: Duration = Duration::from_millis(50);

/// An `agent.output` is sent early once its items reach about this many bytes.
const MAX_BATCH_BYTES: usize = 256 * 1024;

/// What an actor is asked to do.
pub(super) enum Command {
    /// `agent/send`.
    Send {
        turn_id: TurnId,
        text: String,
        /// The message's images, already checked (RYA-191).
        images: Vec<PromptImage>,
        /// The threads attached to it, already checked (PLX-372).
        threads: Vec<RunId>,
        /// New options for the run (RYA-161, RYA-163).
        options: RunOptions,
        /// A new account for the run, perhaps on another backend.
        account: Option<AccountChoice>,
        /// Into the running turn rather than after it (PLX-370).
        steer: bool,
        reply: oneshot::Sender<Result<AgentRun, ErrorObject>>,
    },
    /// `queue/*` (PLX-370).
    Queue {
        op: QueueOp,
        reply: oneshot::Sender<Result<QueueResult, ErrorObject>>,
    },
    /// `agent/cancel`.
    Cancel {
        reply: oneshot::Sender<Result<AgentRun, ErrorObject>>,
    },
    /// `agent/approve` (RYA-222), with its params checked.
    Approve {
        params: AgentApproveParams,
        reply: oneshot::Sender<Result<AgentApproveResult, ErrorObject>>,
    },
    /// `agent/accept`.
    Accept {
        id: AcceptId,
        reviewed: Option<String>,
        reply: oneshot::Sender<Result<(AgentRun, AgentMerge), ErrorObject>>,
    },
    /// `agent/openPr` (RYA-168).
    OpenPr {
        title: String,
        body: String,
        reply: oneshot::Sender<Result<String, ErrorObject>>,
    },
    /// `agent/gitStatus`, `agent/commit`, or `agent/push` (RYA-298).
    Git {
        action: GitAction,
        reply: oneshot::Sender<Result<GitStatus, ErrorObject>>,
    },
    /// `thread/delete` (#110) and `project/delete` (PLX-338): stops the run's CLI, waits for it to
    /// exit, and deletes the run.
    Delete {
        reply: oneshot::Sender<Result<(), ErrorObject>>,
    },
    /// A run this coordinator started finished, as [`wake::summary`] tells it (RYA-42).
    Wake(String),
    /// `agent/resumeNow` (PLX-371).
    ResumeNow {
        reply: oneshot::Sender<Result<AgentRun, ErrorObject>>,
    },
    /// `agent/autoResume` (PLX-371).
    AutoResume {
        auto_resume: Option<bool>,
        reply: oneshot::Sender<Result<AgentRun, ErrorObject>>,
    },
}

impl Command {
    /// Answers the command with `error` without running it.
    fn refuse(self, error: ErrorObject) {
        match self {
            Self::Send { reply, .. }
            | Self::Cancel { reply }
            | Self::ResumeNow { reply }
            | Self::AutoResume { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Accept { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Approve { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Queue { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::OpenPr { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Git { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::Delete { reply } => {
                let _ = reply.send(Err(error));
            }
            Self::Wake(_) => {}
        }
    }
}

/// What `queue/*` asks of the run's queue (PLX-370).
pub(crate) enum QueueOp {
    List,
    Edit { id: TurnId, text: String },
    Reorder { ids: Vec<TurnId> },
    Cancel { id: TurnId },
    Steer { id: TurnId },
}

/// A message waiting for the run's CLI: one sent during a turn, one that changes what the CLI
/// runs with, or one sent after either. Stored, so a restart keeps it (PLX-370).
#[derive(Clone)]
struct Queued {
    turn_id: TurnId,
    text: String,
    images: Vec<PromptImage>,
    threads: Vec<RunId>,
    options: RunOptions,
    account: Option<AccountChoice>,
}

/// What a stored [`Queued`] keeps beside its text, as its row's JSON.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct QueuedExtra {
    images: Vec<PromptImage>,
    threads: Vec<RunId>,
    options: RunOptions,
    account: Option<AccountChoice>,
}

impl Queued {
    fn row(&self) -> QueuedRow {
        let extra = QueuedExtra {
            images: self.images.clone(),
            threads: self.threads.clone(),
            options: self.options.clone(),
            account: self.account.clone(),
        };
        QueuedRow {
            turn_id: self.turn_id.into(),
            text: self.text.clone(),
            // Plain data, which serializes.
            extra: serde_json::to_string(&extra).unwrap_or_default(),
        }
    }

    /// `row` as it was stored, or `None` if it's corrupt.
    fn from_row(row: QueuedRow) -> Option<Self> {
        let turn_id = TurnId::try_from(row.turn_id).ok()?;
        let extra: QueuedExtra = serde_json::from_str(&row.extra).ok()?;
        Some(Self {
            turn_id,
            text: row.text,
            images: extra.images,
            threads: extra.threads,
            options: extra.options,
            account: extra.account,
        })
    }

    /// It, for the live CLI as its next turn, after the summaries of the threads attached to it
    /// (PLX-372).
    async fn follow_up(&self, daemon: &Daemon) -> Result<FollowUp, ErrorObject> {
        Ok(FollowUp {
            turn_id: self.turn_id,
            text: attached::prompt(daemon, &self.threads, &self.text).await?,
            images: self.images.clone(),
            steer: false,
        })
    }

    fn message(&self) -> QueuedMessage {
        QueuedMessage {
            id: self.turn_id,
            text: self.text.clone(),
            images: u32::try_from(self.images.len()).unwrap_or(u32::MAX),
            threads: self.threads.clone(),
        }
    }
}

struct Live {
    run: Arc<dyn Run>,
    events: EventStream,
    /// A worker's temp folder (RYA-130), removed once the CLI has exited: `events` ends only
    /// then.
    temp: Option<RunTemp>,
}

/// What a run's CLI starts with besides its account and prompt, from [`Actor::launch`].
struct Setup {
    cwd: PathBuf,
    sandbox: Option<WorkerSandbox>,
    temp: Option<RunTemp>,
    tools: Option<CoordinatorTools>,
    thread: bool,
}

#[derive(Default)]
struct Batch {
    items: Vec<AgentOutputItem>,
    bytes: usize,
    since: Option<Instant>,
}

pub(super) struct Actor {
    daemon: Arc<Daemon>,
    id: RunId,
    project: ProjectId,
    row: RunRow,
    /// The run's worktree, until `agent/accept` removes it.
    worktree: Option<Worktree>,
    live: Option<Live>,
    batch: Batch,
    /// Messages sent to the run, by turn id, reloaded from the store after a restart. They make
    /// `agent/send` idempotent across CLI processes and fill in the logged `TurnStarted.text`.
    turns: HashMap<TurnId, String>,
    /// The stored images of messages a CLI took, by turn id (`None` for the prompt's), until
    /// their `TurnStarted` lists them (RYA-191, decision 0026).
    images: HashMap<Option<TurnId>, Vec<ImageId>>,
    /// The threads attached to messages a CLI took, by turn id as `images`, until their
    /// `TurnStarted` lists them (PLX-372).
    attached: HashMap<Option<TurnId>, Vec<RunId>>,
    /// The latest prompt or message, for the commit message.
    last_message: String,
    /// Messages waiting for the run's CLI, first to be sent first (PLX-370).
    queued: VecDeque<Queued>,
    /// Turns the live CLI has been given and hasn't finished: while there are any, a queued
    /// message waits.
    in_flight: usize,
    /// What [`Run::hold`] last told the live CLI.
    held: bool,
    /// Messages the live CLI took and hasn't started a turn for: one it drops instead, as a CLI
    /// that exits first does, waits again for the next CLI.
    handed: Vec<Queued>,
    stopping: bool,
    /// Set once `thread/delete` or `project/delete` removed the run: the actor stops, refusing
    /// what is still queued.
    deleted: bool,
    /// A coordinator's wake-ups (RYA-42).
    wakes: Wakes,
    /// What a usage limit's resume needs (PLX-371).
    resumes: Resumes,
    /// The permission requests its CLIs asked (RYA-222).
    approvals: Approvals,
    /// The tool calls running `gh pr create`, by call id, until their results link the pull
    /// requests they print (PLX-318).
    pr_calls: HashSet<String>,
}

impl Actor {
    /// `turns` is what a run already sent, from the store (#190): empty for a run just created by
    /// `agents::start`, and loaded by `actor_for` for a run whose actor is spawned fresh, so a
    /// restarted plxd still recognizes a retried `agent/send`.
    pub fn new(
        daemon: Arc<Daemon>,
        row: RunRow,
        worktree: Option<Worktree>,
        turns: HashMap<TurnId, String>,
    ) -> Self {
        let id = RunId::try_from(row.id).unwrap_or_else(|_| RunId::generate());
        let project = ProjectId::try_from(row.fields.project_id).unwrap_or_else(|_| {
            warn!(run = %row.id, "a stored run's project id is not a UUIDv7");
            ProjectId::generate()
        });
        let last_message = row.fields.prompt.clone();
        Self {
            daemon,
            id,
            project,
            row,
            worktree,
            live: None,
            batch: Batch::default(),
            turns,
            images: HashMap::new(),
            attached: HashMap::new(),
            last_message,
            queued: VecDeque::new(),
            in_flight: 0,
            held: false,
            handed: Vec::new(),
            stopping: false,
            deleted: false,
            wakes: Wakes::default(),
            resumes: Resumes::default(),
            approvals: Approvals::default(),
            pr_calls: HashSet::new(),
        }
    }

    pub fn id(&self) -> RunId {
        self.id
    }

    pub fn snapshot(&self) -> Result<AgentRun, ErrorObject> {
        agent_run(&self.row, self.worktree.as_ref())
    }

    fn accepted(&self) -> bool {
        self.row.state.status == convert::ACCEPTED
    }

    /// Whether this is a project's coordinator (0024) rather than a worker or a thread.
    fn is_coordinator(&self) -> bool {
        self.row.fields.policy == convert::NO_WRITE
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>, shutdown: CancellationToken) {
        if self.is_coordinator() {
            self.load_wakes().await;
        }
        self.load_queue().await;
        loop {
            self.deliver().await;
            let deadline = self.batch.since.map(|since| since + COALESCE);
            // A coordinator's turn in progress gets its wake-ups next, once its CLI has exited.
            let wake_at = self.wakes.due().filter(|_| self.live.is_none());
            let expire_at = self.approvals.due();
            let resume_at = self.resume_due();
            tokio::select! {
                // Shutdown, then a command, then the due flush, and only then another backend
                // event (#190 N6): while a CLI keeps its stream busy, that event branch is
                // otherwise always ready, and `biased` would starve `agent/cancel` and the
                // coalescing flush for as long as the flood lasts, rather than just until the
                // next iteration. Side effect (#190 review, non-blocking): a command can now run
                // before a backend event still buffered ahead of it, so `agent/accept` can see a
                // transient `mergeRefused` for a run whose CLI has already exited but whose
                // `Finished` hasn't been drained yet. `send` already copes with the equivalent
                // case (`SendError::Finished`); a caller of `accept` just retries.
                biased;
                // What waits stays stored, for the next plxd to send.
                () = shutdown.cancelled(), if !self.stopping => {
                    self.stopping = true;
                    self.stop_approvals(AgentApprovalBy::Stop).await;
                    if let Some(live) = &self.live {
                        live.run.cancel();
                    }
                }
                command = commands.recv(), if !self.stopping => match command {
                    Some(command) => self.on_command(command).await,
                    None => self.stopping = true,
                },
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.flush().await;
                }
                () = sleep_until(wake_at.unwrap_or_else(Instant::now)), if wake_at.is_some() => {
                    self.wake().await;
                }
                () = sleep_until(expire_at.unwrap_or_else(Instant::now)), if expire_at.is_some() => {
                    self.expire_approvals().await;
                }
                () = sleep_until(resume_at.unwrap_or_else(Instant::now)), if resume_at.is_some() => {
                    self.check_resume().await;
                }
                event = next_event(&mut self.live) => self.on_event(event).await,
            }
            if self.stopping && self.live.is_none() {
                self.flush().await;
                break;
            }
        }
        if self.deleted {
            commands.close();
            while let Ok(command) = commands.try_recv() {
                command.refuse(super::run_not_found(self.id));
            }
        }
    }

    async fn on_command(&mut self, command: Command) {
        match command {
            Command::Send {
                turn_id,
                text,
                images,
                threads,
                options,
                account,
                steer,
                reply,
            } => {
                let message = Queued {
                    turn_id,
                    text,
                    images,
                    threads,
                    options,
                    account,
                };
                let answer = self.send(message, steer).await;
                if answer.is_ok() && self.wakes.attended() {
                    self.save_wakes().await;
                }
                let _ = reply.send(answer);
            }
            Command::Cancel { reply } => {
                if self.live.is_some() {
                    info!(run = %self.id, "cancelling an agent run");
                    self.stop_approvals(AgentApprovalBy::Cancel).await;
                }
                // Stop means stop: what waited for this turn to end doesn't start another, nor
                // does what the CLI took and drops as it stops, and a run waiting for its usage
                // limit doesn't resume.
                self.handed.clear();
                self.drop_queued().await;
                self.cancel_waiting().await;
                if let Some(live) = &self.live {
                    live.run.cancel();
                }
                // Stop means stop: a run finishing a moment later doesn't start the coordinator
                // again before the user writes.
                if self.is_coordinator() {
                    self.pause_wakes().await;
                }
                let _ = reply.send(self.snapshot());
            }
            Command::Approve { params, reply } => {
                let answer = self.approve(params).await;
                let _ = reply.send(answer);
            }
            Command::Queue { op, reply } => {
                let answer = self.queue_op(op).await;
                let _ = reply.send(answer);
            }
            Command::Accept {
                id,
                reviewed,
                reply,
            } => {
                let answer = self.accept(id, reviewed).await;
                let _ = reply.send(answer);
            }
            Command::OpenPr { title, body, reply } => {
                let answer = self.open_pr(&title, &body).await;
                if let Ok(url) = &answer {
                    self.link_pr(url.clone()).await;
                }
                let _ = reply.send(answer);
            }
            Command::Git { action, reply } => {
                let answer = self.git(action).await;
                let _ = reply.send(answer);
            }
            Command::Delete { reply } => {
                let answer = self.delete().await;
                if answer.is_ok() {
                    self.deleted = true;
                    self.stopping = true;
                }
                let _ = reply.send(answer);
            }
            Command::Wake(summary) => {
                if self.is_coordinator() {
                    self.wakes.push(summary, Instant::now());
                }
            }
            Command::ResumeNow { reply } => {
                let _ = reply.send(self.resume_now().await);
            }
            Command::AutoResume { auto_resume, reply } => {
                let _ = reply.send(self.set_auto_resume(auto_resume).await);
            }
        }
    }

    /// Sends what is waiting as the coordinator's next turn, through the same resume as
    /// `agent/send` (RYA-42). Pauses wake-ups at the cap, or when this fails, keeping what is
    /// waiting. Only the project's current coordinator wakes: a replaced one drops them, so a
    /// project never has two live (0024).
    async fn wake(&mut self) {
        let project = self.project.into();
        let current = store(&self.daemon, move |db| {
            super::coordinator::coordinator_of(db, project)
        })
        .await;
        match current {
            Ok(Some(current)) if current == self.id => {}
            Ok(_) => {
                self.wakes.clear();
                return;
            }
            Err(error) => {
                warn!(run = %self.id, error = %error.message, "could not check a coordinator before waking it");
                self.pause_wakes().await;
                return;
            }
        }
        let Some((turn_id, text)) = self.wakes.next() else {
            self.pause_wakes().await;
            return;
        };
        info!(run = %self.id, "waking a coordinator: runs it started finished");
        match self
            .resume(
                turn_id,
                text,
                Vec::new(),
                Vec::new(),
                RunOptions::default(),
                None,
            )
            .await
        {
            Ok(_) if self.live.is_some() => {
                self.wakes.delivered();
                self.save_wakes().await;
            }
            Ok(_) => self.pause_wakes().await,
            Err(error) => {
                warn!(run = %self.id, error = %error.message, "could not wake a coordinator");
                self.pause_wakes().await;
            }
        }
    }

    /// Stops waking the coordinator until the user writes, and says so once (RYA-42).
    async fn pause_wakes(&mut self) {
        if self.wakes.pause() {
            info!(run = %self.id, "pausing a coordinator's wake-ups until the user writes");
            self.save_wakes().await;
            self.append(ParallaxEvent::AgentWakeupsPaused { run_id: self.id })
                .await;
        }
    }

    /// Takes up the coordinator's wake-up count and pause where the last plxd left them
    /// (RYA-178). If they can't be read, pauses wake-ups, as a failed check does.
    async fn load_wakes(&mut self) {
        let id = self.row.id;
        let stored = store(&self.daemon, move |db| {
            db.wake_state(id).map_err(|error| store_error(&error))
        })
        .await;
        match stored {
            Ok(state) => self.wakes.restore(state),
            Err(error) => {
                warn!(run = %self.id, error = %error.message, "could not read a coordinator's wake-ups");
                self.pause_wakes().await;
            }
        }
    }

    /// Stores the coordinator's wake-up count and pause, so a restart keeps them (RYA-178).
    async fn save_wakes(&self) {
        let (id, state) = (self.row.id, self.wakes.state());
        let saved = store(&self.daemon, move |db| {
            db.set_wake_state(id, state)
                .map_err(|error| store_error(&error))
        })
        .await;
        if let Err(error) = saved {
            warn!(run = %self.id, error = %error.message, "could not store a coordinator's wake-ups");
        }
    }

    /// `thread/delete` and `project/delete`: cancels a running CLI and waits for it to exit and
    /// its changes to be committed, then deletes the run's rows, events, worktree, and a thread's
    /// scratch folders ([`crate::threads::purge`]), and drops this actor from the map. Running
    /// here, between commands, it never races a resume or an accept.
    async fn delete(&mut self) -> Result<(), ErrorObject> {
        if self.live.is_some() {
            self.stop_approvals(AgentApprovalBy::Cancel).await;
        }
        if let Some(live) = &self.live {
            info!(run = %self.id, "cancelling an agent run to delete it");
            live.run.cancel();
            while self.live.is_some() {
                let event = next_event(&mut self.live).await;
                self.on_event(event).await;
            }
        }
        self.flush().await;
        // Holds off a concurrent create/actor_for retry for this exact run id while its rows are
        // deleted and this actor is dropped (#110); an unrelated run's own lock is untouched.
        let _creating = self.daemon.agents.start_guard(self.id).await;
        crate::threads::purge(&self.daemon, self.id, self.worktree.clone()).await?;
        self.worktree = None;
        self.daemon.agents.forget(self.id);
        Ok(())
    }

    /// `agent/accept`: merges the run's latest commit into the project's current branch, removes
    /// its worktree and branch, and records it `accepted` (#157, #68).
    async fn accept(
        &mut self,
        id: AcceptId,
        reviewed: Option<String>,
    ) -> Result<(AgentRun, AgentMerge), ErrorObject> {
        if let Some(accept) = &self.row.state.accept {
            if accept.id == Uuid::from(id) {
                return Ok((self.snapshot()?, convert::merge(accept)));
            }
            return Err(super::run_accepted(self.id));
        }
        let refused = |why: String| ErrorObject::parallax(ErrorKind::MergeRefused, why);
        if self.live.is_some() {
            return Err(refused(format!(
                "run {} is still running; wait for it to finish, or cancel it, then accept",
                self.id
            )));
        }
        let Some(commit) = self.row.state.commit_sha.clone() else {
            return Err(refused(format!(
                "run {} has no committed changes to accept",
                self.id
            )));
        };
        if let Some(reviewed) = reviewed
            && reviewed != commit
        {
            return Err(refused(format!(
                "run {} has committed new changes since {reviewed}, the commit you reviewed; \
                 review {commit} before accepting",
                self.id
            )));
        }
        let Some(worktree) = self.worktree.clone() else {
            return Err(ErrorObject::internal_error(format!(
                "run {} has no recorded worktree",
                self.id
            )));
        };
        let worktrees = &self.daemon.agents.worktrees;
        let repo = Path::new(&worktree.repo_path);
        let message = merge_message(&self.row.fields.prompt, self.id, &worktree.branch);
        let accepted = worktrees
            .accept(repo, &commit, &message)
            .await
            .map_err(|error| accept_error(&error))?;
        info!(run = %self.id, into = %accepted.into, commit = %accepted.commit, "accepted an agent run");
        if let Err(error) = worktrees
            .remove(repo, Path::new(&worktree.path), &worktree.branch)
            .await
        {
            warn!(run = %self.id, %error, "could not remove an accepted run's worktree");
        }

        let accept = RunAccept {
            id: id.into(),
            commit: accepted.commit,
            into: accepted.into,
            how: convert::merge_how_text(accepted.how).to_owned(),
        };
        convert::ACCEPTED.clone_into(&mut self.row.state.status);
        self.row.state.error = None;
        self.row.state.resume_at = None;
        self.row.state.accept = Some(accept.clone());
        let (row_id, state) = (self.row.id, self.row.state.clone());
        let saved = store(&self.daemon, move |db| {
            db.accept_run(row_id, &state)
                .map_err(|error| store_error(&error))
        })
        .await;
        match saved {
            Ok(row) => self.row = row,
            Err(error) => {
                warn!(run = %self.id, error = %error.message, "could not store an accepted run");
            }
        }
        self.worktree = None;
        let merge = convert::merge(&accept);
        self.flush().await;
        self.append(ParallaxEvent::AgentAccepted {
            run_id: self.id,
            merge: merge.clone(),
        })
        .await;
        self.append(ParallaxEvent::AgentUpdated {
            run_id: self.id,
            state: convert::run_state(&self.row),
        })
        .await;
        Ok((self.snapshot()?, merge))
    }

    /// `agent/openPr`: pushes the run's branch to its repository's `origin` and returns the URL
    /// of its pull request, opening one if none is open (RYA-168). Running here, between commands,
    /// it never races a turn or its commit.
    async fn open_pr(&self, title: &str, body: &str) -> Result<String, ErrorObject> {
        if self.accepted() {
            return Err(super::run_accepted(self.id));
        }
        let refused = |why: String| ErrorObject::parallax(ErrorKind::PrRefused, why);
        if self.live.is_some() {
            return Err(refused(format!(
                "run {} is still running; open a pull request once it has finished",
                self.id
            )));
        }
        // A Current checkout thread pushes the branch its checkout has out (RYA-298).
        if self.row.fields.checkout {
            let repo = self.checkout_path().await?;
            let worktrees = &self.daemon.agents.worktrees;
            let branch = worktrees
                .current_branch(&repo)
                .await
                .map_err(|error| ErrorObject::parallax(ErrorKind::PushFailed, error.to_string()))?
                .ok_or_else(|| {
                    refused(format!(
                        "run {}'s checkout has a detached HEAD; check out a branch to open a pull \
                         request",
                        self.id
                    ))
                })?;
            return self.pull_request(&repo, &branch, title, body).await;
        }
        if self.row.state.commit_sha.is_none() {
            return Err(refused(format!(
                "run {} has no committed changes to open a pull request for",
                self.id
            )));
        }
        let project = self.project;
        if store(&self.daemon, move |db| {
            crate::threads::is_scratch(db, project)
        })
        .await?
        {
            return Err(refused(format!(
                "run {} is a thread with no repository, so it has no origin to push to",
                self.id
            )));
        }
        let Some(worktree) = &self.worktree else {
            return Err(ErrorObject::internal_error(format!(
                "run {} has no recorded worktree",
                self.id
            )));
        };
        self.pull_request(
            Path::new(&worktree.repo_path),
            &worktree.branch,
            title,
            body,
        )
        .await
    }

    /// Pushes `branch` from `repo` and returns its pull request's URL, for `agent/openPr`.
    async fn pull_request(
        &self,
        repo: &Path,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String, ErrorObject> {
        let url = self
            .daemon
            .agents
            .worktrees
            .open_pr(repo, branch, title, body)
            .await
            .map_err(|error| super::pr_error(&error))?;
        info!(run = %self.id, %url, "opened a pull request for an agent run");
        Ok(url)
    }

    /// Links pull request `url` to the run, unless it already is, and reports it as
    /// `agent.updated` (PLX-318).
    async fn link_pr(&mut self, url: String) {
        if self.row.state.pull_requests.contains(&url) {
            return;
        }
        info!(run = %self.id, %url, "linked a pull request to an agent run");
        self.row.state.pull_requests.push(url);
        self.save().await;
    }

    /// The pull requests a finished `gh pr create` tool call printed, whatever the backend: its
    /// call is told by its input's JSON text, not by the tool's name, and remembered until its
    /// result.
    fn created_prs(&mut self, event: &Event) -> Vec<String> {
        match event {
            Event::ToolCall { call_id, input, .. }
                if input.to_string().contains("gh pr create") =>
            {
                self.pr_calls.insert(call_id.clone());
                Vec::new()
            }
            Event::ToolResult {
                call_id, output, ..
            } if self.pr_calls.remove(call_id) => {
                output.as_deref().map(github_pr_urls).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    /// `agent/approve` (RYA-222): passes the user's answer to the CLI and logs it as the
    /// request's resolution. A request that already ended answers with how it ended.
    async fn approve(
        &mut self,
        params: AgentApproveParams,
    ) -> Result<AgentApproveResult, ErrorObject> {
        let AgentApproveParams {
            approval_id,
            decision,
            input,
            always,
            message,
            ..
        } = params;
        match self.approvals.lookup(approval_id) {
            Lookup::Resolved(resolution) => return Ok(resolution),
            Lookup::Unknown => return Err(super::approval_not_found(self.id, approval_id)),
            Lookup::Pending { offers_always, .. } if always && !offers_always => {
                return Err(ErrorObject::invalid_params(format!(
                    "permission request {approval_id} offers no rules to always allow"
                )));
            }
            // Claude Code may not hold an edited input to its worker's confinement (0031).
            Lookup::Pending { paths, .. }
                if self.row.fields.policy == WORKSPACE_WRITE
                    && input
                        .as_ref()
                        .is_some_and(|edited| approvals::moves_paths(&paths, edited)) =>
            {
                return Err(ErrorObject::invalid_params(format!(
                    "an edit to permission request {approval_id} must keep its {}, and may \
                     leave out its {} but not change it",
                    approvals::PATH_FIELDS.join(", "),
                    approvals::PLAN_PATH_FIELD,
                )));
            }
            Lookup::Pending { .. } => {}
        }
        let allow = decision == AgentApprovalAnswer::Allow;
        let answer = Answer {
            approval_id,
            decision: if allow {
                Decision::Allow { input, always }
            } else {
                Decision::Deny {
                    message: message
                        .clone()
                        .unwrap_or_else(|| approvals::DENIED.to_owned()),
                    interrupt: false,
                }
            },
        };
        let sent = match &self.live {
            Some(live) => live.run.answer(answer),
            None => Err(AnswerError::Finished),
        };
        if sent.is_ok() {
            let resolution = if allow {
                AgentApproveResult {
                    decision: AgentApprovalDecision::Allowed,
                    by: AgentApprovalBy::User,
                    always,
                    message: None,
                }
            } else {
                AgentApproveResult {
                    decision: AgentApprovalDecision::Denied,
                    by: AgentApprovalBy::User,
                    always: false,
                    message,
                }
            };
            self.resolve_approval(approval_id, resolution.clone()).await;
            self.flush().await;
            return Ok(resolution);
        }
        // The CLI that asked no longer waits: it exited, perhaps with a fallback attempt running
        // in its place (#119), so the request ends as the CLI's exit ends it. Waiting for the
        // run's end here would hold up this actor for the fallback attempt's whole run.
        let withdrawn = ended(AgentApprovalDecision::Withdrawn, AgentApprovalBy::Agent);
        self.resolve_approval(approval_id, withdrawn).await;
        self.flush().await;
        match self.approvals.lookup(approval_id) {
            Lookup::Resolved(resolution) => Ok(resolution),
            Lookup::Pending { .. } | Lookup::Unknown => Err(ErrorObject::internal_error(format!(
                "permission request {approval_id} was left unresolved"
            ))),
        }
    }

    /// Denies every permission request nobody answered in time (RYA-222).
    async fn expire_approvals(&mut self) {
        for approval_id in self.approvals.expired(Instant::now()) {
            info!(run = %self.id, approval = %approval_id, "a permission request expired");
            if let Some(live) = &self.live {
                let decision = Decision::Deny {
                    message: approvals::EXPIRED.to_owned(),
                    interrupt: false,
                };
                let _ = live.run.answer(Answer {
                    approval_id,
                    decision,
                });
            }
            let expired = ended(AgentApprovalDecision::Expired, AgentApprovalBy::Timeout);
            self.resolve_approval(approval_id, expired).await;
        }
    }

    /// Denies every waiting permission request, ending the CLI's turn as well, because `by` is
    /// stopping the run.
    async fn stop_approvals(&mut self, by: AgentApprovalBy) {
        for approval_id in self.approvals.pending() {
            if let Some(live) = &self.live {
                let decision = Decision::Deny {
                    message: approvals::STOPPED.to_owned(),
                    interrupt: true,
                };
                let _ = live.run.answer(Answer {
                    approval_id,
                    decision,
                });
            }
            let stopped = ended(AgentApprovalDecision::Denied, by);
            self.resolve_approval(approval_id, stopped).await;
        }
    }

    /// Records how a waiting permission request ended, and logs it as `approvalResolved`. Does
    /// nothing to one that already ended.
    async fn resolve_approval(&mut self, approval_id: ApprovalId, resolution: AgentApproveResult) {
        let item = convert::approval_resolved(approval_id, &resolution);
        if self.approvals.resolve(approval_id, resolution) {
            self.push(item).await;
        }
    }

    /// `agent/send`: `queued` goes into the running turn with `steer`, and otherwise as the
    /// run's next turn, waiting for it if it must.
    async fn send(&mut self, queued: Queued, steer: bool) -> Result<AgentRun, ErrorObject> {
        let (turn_id, text) = (queued.turn_id, &queued.text);
        if text.trim().is_empty() && queued.images.is_empty() {
            return Err(ErrorObject::invalid_params("text must not be empty"));
        }
        if self.accepted() {
            return Err(super::run_accepted(self.id));
        }
        let waiting = self.queued.iter().find(|queued| queued.turn_id == turn_id);
        if let Some(sent) = self
            .turns
            .get(&turn_id)
            .or(waiting.map(|queued| &queued.text))
        {
            return if sent == text {
                self.snapshot()
            } else {
                Err(id_conflict(turn_id))
            };
        }
        if steer {
            return self.steer(queued).await;
        }
        // A running CLI can't change what it runs with, a turn in progress finishes before the
        // next starts, and what's sent after a message that waits waits too, so the messages keep
        // their order.
        if self.live.is_some()
            && (self.changing(&queued) || self.in_flight > 0 || !self.queued.is_empty())
        {
            return self.queue(queued).await;
        }
        if self.live.is_some() {
            match self.hand_over(&queued).await? {
                Ok(()) => {
                    self.handed_over(queued).await;
                    return self.snapshot();
                }
                Err(SendError::IdConflict) => return Err(id_conflict(turn_id)),
                // A backend that takes no messages while it runs gets this one once it's done.
                Err(SendError::Unsupported) => return self.queue(queued).await,
                // The CLI is exiting: let the run finish, then resume it with the message.
                Err(SendError::Finished) => self.drain().await,
            }
        }
        let Queued {
            turn_id,
            text,
            images,
            threads,
            options,
            account,
        } = queued;
        let changes = self.changes(options);
        self.resume(turn_id, text, images, threads, changes, account)
            .await
    }

    /// `agent/send` with `delivery: steer`, and `queue/steer`: `queued` goes into the turn running
    /// now (PLX-370). A backend that takes no messages while it runs is cancelled and resumed
    /// with it, and a run with no CLI running resumes with it at once.
    async fn steer(&mut self, queued: Queued) -> Result<AgentRun, ErrorObject> {
        if self.changing(&queued) {
            return Err(ErrorObject::parallax(
                ErrorKind::UnsupportedOption,
                "a steer goes into the running turn, which can't change the run's model, \
                 options, or account; queue the message instead",
            ));
        }
        let mut steer = queued.follow_up(&self.daemon).await?;
        steer.steer = true;
        let sent = self.live.as_ref().map(|live| live.run.send(steer));
        match sent {
            Some(Ok(())) => {
                info!(run = %self.id, turn = %queued.turn_id, "steering a running turn");
                self.handed_over(queued).await;
                return self.snapshot();
            }
            Some(Err(SendError::IdConflict)) => return Err(id_conflict(queued.turn_id)),
            Some(Err(SendError::Unsupported)) => {
                info!(run = %self.id, turn = %queued.turn_id, "interrupting a run to steer it");
                self.stop_approvals(AgentApprovalBy::Cancel).await;
                if let Some(live) = &self.live {
                    live.run.cancel();
                }
                self.drain().await;
            }
            Some(Err(SendError::Finished)) => self.drain().await,
            None => {}
        }
        let Queued {
            turn_id,
            text,
            images,
            threads,
            ..
        } = queued;
        self.resume(turn_id, text, images, threads, RunOptions::default(), None)
            .await
    }

    /// Hands `queued` to the live CLI as its next turn.
    async fn hand_over(&self, queued: &Queued) -> Result<Result<(), SendError>, ErrorObject> {
        let follow_up = queued.follow_up(&self.daemon).await?;
        Ok(match &self.live {
            Some(live) => live.run.send(follow_up),
            None => Err(SendError::Finished),
        })
    }

    /// Records `queued`, which the live CLI has taken.
    async fn handed_over(&mut self, queued: Queued) {
        self.in_flight += 1;
        self.handed.push(queued.clone());
        self.record_turn(queued.turn_id, queued.text.clone()).await;
        self.keep_images(Some(queued.turn_id), queued.images).await;
        self.attach(Some(queued.turn_id), queued.threads);
        self.last_message = queued.text;
    }

    /// Lets the live CLI, which is exiting or was cancelled, finish.
    async fn drain(&mut self) {
        while self.live.is_some() {
            let event = next_event(&mut self.live).await;
            self.on_event(event).await;
        }
    }

    /// Whether `queued` changes what the run's CLI runs with, so it waits for the CLI to exit.
    fn changing(&self, queued: &Queued) -> bool {
        self.changes(queued.options.clone()) != RunOptions::default()
            || self.moves(queued.account.as_ref())
    }

    /// Of `options`, those that differ from the run's.
    fn changes(&self, options: RunOptions) -> RunOptions {
        let fields = &self.row.fields;
        RunOptions {
            model: options.model.filter(|m| Some(m) != fields.model.as_ref()),
            effort: options.effort.filter(|&e| option_name(e) != fields.effort),
            permission: options
                .permission
                .filter(|&p| option_name(p) != fields.permission),
            context_window: options
                .context_window
                .filter(|&w| Some(w) != fields.context_window),
            fast: options.fast.filter(|&f| Some(f) != fields.fast),
        }
    }

    /// Whether `account` is another account than the one the run's session is on.
    fn moves(&self, account: Option<&AccountChoice>) -> bool {
        account.is_some_and(|account| *account != session_account(&self.row.state.account_id))
    }

    /// Keeps a message until the run's CLI can take it.
    async fn queue(&mut self, queued: Queued) -> Result<AgentRun, ErrorObject> {
        info!(run = %self.id, turn = %queued.turn_id, "a message waits for the run's CLI");
        self.queued.push_back(queued);
        // A message plxd couldn't store must not look queued.
        if let Err(error) = self.store_queue().await {
            self.queued.pop_back();
            return Err(error);
        }
        self.report_queue().await;
        self.snapshot()
    }

    /// Sends what waits as far as the run can take it now: while no CLI runs, the next message
    /// to a new CLI process; while the live CLI has no turn in progress, the next message that
    /// doesn't change what it runs with, as its next turn. Holds the CLI open while that waits.
    async fn deliver(&mut self) {
        if self.stopping {
            return;
        }
        if self.live.is_none() {
            self.send_queued().await;
        }
        while self.in_flight == 0 && self.live.is_some() {
            let Some(next) = self.queued.front().cloned() else {
                break;
            };
            if self.changing(&next) {
                break;
            }
            // At least once (0048): the CLI gets the message before its stored row is deleted,
            // so a crash between the two resends it once after a restart, as `record_turn` does,
            // rather than losing it.
            let handed = match self.hand_over(&next).await {
                Ok(handed) => handed,
                Err(error) => {
                    self.queued.pop_front();
                    self.dropped(next.turn_id, &error.message).await;
                    self.save_queue().await;
                    continue;
                }
            };
            match handed {
                Ok(()) => {
                    self.queued.pop_front();
                    self.handed_over(next).await;
                    self.save_queue().await;
                }
                Err(SendError::IdConflict) => {
                    self.queued.pop_front();
                    self.dropped(next.turn_id, "its turn id was already used")
                        .await;
                    self.save_queue().await;
                }
                // It goes once the CLI has exited.
                Err(SendError::Unsupported | SendError::Finished) => break,
            }
        }
        let hold =
            self.live.is_some() && self.queued.front().is_some_and(|next| !self.changing(next));
        if let Some(live) = &self.live
            && hold != self.held
        {
            live.run.hold(hold);
            self.held = hold;
        }
    }

    /// Sends the next waiting message, now that no CLI runs, to a new CLI process with its
    /// changes; those after it wait for that process in turn. A message that can't be sent is
    /// dropped, and the transcript says why, and the next is tried.
    async fn send_queued(&mut self) {
        while self.live.is_none() {
            let Some(next) = self.queued.pop_front() else {
                return;
            };
            let Queued {
                turn_id,
                text,
                images,
                threads,
                options,
                account,
            } = next;
            let changes = self.changes(options);
            let why = match self
                .resume(turn_id, text, images, threads, changes, account)
                .await
            {
                Ok(_) if self.live.is_some() => None,
                Ok(_) => Some(
                    self.row
                        .state
                        .error
                        .clone()
                        .unwrap_or_else(|| "its CLI didn't start".to_owned()),
                ),
                Err(error) => Some(error.message),
            };
            if let Some(why) = why {
                self.dropped(turn_id, &why).await;
            }
            self.save_queue().await;
        }
    }

    /// Logs that waiting message `turn_id` couldn't be sent, and why.
    async fn dropped(&mut self, turn_id: TurnId, why: &str) {
        warn!(run = %self.id, turn = %turn_id, %why, "a waiting message couldn't be sent");
        self.push(AgentOutputItem::Warning {
            detail: format!("A message couldn't be sent: {why}"),
        })
        .await;
        self.push(AgentOutputItem::FollowUpDropped { turn_id })
            .await;
        self.flush().await;
    }

    /// Drops every waiting message, which never reached a CLI, as a stopped run's follow-ups are.
    async fn drop_queued(&mut self) {
        if self.queued.is_empty() {
            return;
        }
        while let Some(queued) = self.queued.pop_front() {
            info!(run = %self.id, turn = %queued.turn_id, "dropping a waiting message");
            let turn_id = queued.turn_id;
            self.push(AgentOutputItem::FollowUpDropped { turn_id })
                .await;
        }
        self.save_queue().await;
    }

    /// `queue/*` (PLX-370): reads or changes the waiting messages, and answers with them as they
    /// are after.
    async fn queue_op(&mut self, op: QueueOp) -> Result<QueueResult, ErrorObject> {
        match op {
            QueueOp::List => {}
            QueueOp::Edit { id, text } => {
                let at = self.position(id)?;
                let queued = &mut self.queued[at];
                if text.trim().is_empty() && queued.images.is_empty() {
                    return Err(ErrorObject::invalid_params("text must not be empty"));
                }
                queued.text = text;
                self.save_queue().await;
            }
            QueueOp::Reorder { ids } => {
                let mut rest = self.queued.clone();
                let mut reordered = VecDeque::with_capacity(rest.len());
                for id in ids {
                    let at = rest.iter().position(|queued| queued.turn_id == id);
                    let Some(queued) = at.and_then(|at| rest.remove(at)) else {
                        return Err(ErrorObject::invalid_params(format!(
                            "ids must list each waiting message once, and {id} isn't one or is \
                             listed twice"
                        )));
                    };
                    reordered.push_back(queued);
                }
                if !rest.is_empty() {
                    return Err(ErrorObject::invalid_params(format!(
                        "ids must list each waiting message once, and leave out {}",
                        rest.len()
                    )));
                }
                self.queued = reordered;
                self.save_queue().await;
            }
            QueueOp::Cancel { id } => {
                let at = self.position(id)?;
                self.queued.remove(at);
                info!(run = %self.id, turn = %id, "cancelling a waiting message");
                self.push(AgentOutputItem::FollowUpDropped { turn_id: id })
                    .await;
                self.save_queue().await;
            }
            QueueOp::Steer { id } => {
                let at = self.position(id)?;
                let queued = self.queued[at].clone();
                if self.changing(&queued) {
                    // `steer` refuses it; the message keeps its place.
                    self.steer(queued).await?;
                } else {
                    self.queued.remove(at);
                    let steered = self.steer(queued.clone()).await;
                    if let Err(error) = steered {
                        let at = at.min(self.queued.len());
                        self.queued.insert(at, queued);
                        return Err(error);
                    }
                    self.save_queue().await;
                }
            }
        }
        Ok(QueueResult {
            messages: self.messages(),
        })
    }

    /// Where waiting message `id` is in the queue.
    fn position(&self, id: TurnId) -> Result<usize, ErrorObject> {
        self.queued
            .iter()
            .position(|queued| queued.turn_id == id)
            .ok_or_else(|| {
                ErrorObject::parallax(
                    ErrorKind::QueuedMessageNotFound,
                    format!("run {} has no waiting message {id}", self.id),
                )
            })
    }

    fn messages(&self) -> Vec<QueuedMessage> {
        self.queued.iter().map(Queued::message).collect()
    }

    /// Takes up the waiting messages the store has for the run, which a plxd before this one
    /// left (PLX-370). One that can't be read is left out, with a warning.
    async fn load_queue(&mut self) {
        let id = self.row.id;
        let stored = store(&self.daemon, move |db| {
            db.queue(id).map_err(|error| store_error(&error))
        })
        .await;
        match stored {
            Ok(rows) => {
                for row in rows {
                    match Queued::from_row(row) {
                        Some(queued) => self.queued.push_back(queued),
                        None => {
                            warn!(run = %self.id, "a stored waiting message is corrupt; leaving it out");
                        }
                    }
                }
            }
            Err(error) => {
                warn!(run = %self.id, error = %error.message, "could not read a run's waiting messages");
            }
        }
    }

    /// Stores the waiting messages as they are now, and reports them as `queue.updated`.
    async fn save_queue(&mut self) {
        if let Err(error) = self.store_queue().await {
            warn!(run = %self.id, error = %error.message, "could not store a run's waiting messages");
        }
        self.report_queue().await;
    }

    /// Stores the waiting messages as they are now.
    async fn store_queue(&self) -> Result<(), ErrorObject> {
        let id = self.row.id;
        let rows: Vec<QueuedRow> = self.queued.iter().map(Queued::row).collect();
        store(&self.daemon, move |db| {
            db.set_queue(id, &rows).map_err(|error| store_error(&error))
        })
        .await
    }

    /// Reports the waiting messages as they are now as `queue.updated`.
    async fn report_queue(&mut self) {
        self.flush().await;
        self.append(ParallaxEvent::QueueUpdated {
            run_id: self.id,
            messages: self.messages(),
        })
        .await;
    }

    /// Starts a new CLI process for the run with `text`, after the summaries of `threads`, and
    /// `images`, after storing `changes` to its options, which the new process runs with. It
    /// resumes the run's vendor session, on `account` if that's another of the same backend's. On
    /// another backend's account, or with no session to resume, a new session starts, told the
    /// conversation so far.
    async fn resume(
        &mut self,
        turn_id: TurnId,
        text: String,
        images: Vec<PromptImage>,
        threads: Vec<RunId>,
        changes: RunOptions,
        account: Option<AccountChoice>,
    ) -> Result<AgentRun, ErrorObject> {
        let moving = self.moves(account.as_ref());
        let session_id = self.row.state.session_id.clone();
        // The session belongs to the account the run was on when it ended, after any fallback,
        // not to whatever the worker role's default is now.
        let account = match account {
            Some(account) if moving => account,
            _ => session_account(&self.row.state.account_id),
        };
        let not_resumable = |why: String| {
            ErrorObject::parallax(
                ErrorKind::RunNotResumable,
                format!("run {} can't be resumed: {why}", self.id),
            )
        };
        let role = if self.is_coordinator() {
            // A replaced coordinator stays stopped: a project has one live coordinator (0024).
            let project = self.project;
            let current = store(&self.daemon, move |db| {
                super::coordinator::coordinator_of(db, project.into())
            })
            .await?;
            if let Some(current) = current.filter(|current| *current != self.id) {
                return Err(not_resumable(format!(
                    "project {project}'s coordinator is now run {current}"
                )));
            }
            Role::Coordinator
        } else {
            Role::Worker
        };
        let (prepared, repo_path) =
            match prepare(&self.daemon, self.project, self.id, Some(account), role).await {
                Ok(prepared) => prepared,
                Err(error)
                    if !moving
                        && error
                            .parallax_data()
                            .is_some_and(|data| data.kind == ErrorKind::AccountNotFound) =>
                {
                    return Err(not_resumable(format!(
                        "its session's account {} no longer exists",
                        self.row.state.account_id
                    )));
                }
                Err(error) => return Err(error),
            };
        let backend = prepared.resolved.backend().name();
        let from = self.row.fields.backend.clone();
        if backend != from && !moving {
            return Err(not_resumable(format!(
                "its session ran on {from}, but its account now runs on {backend}"
            )));
        }
        let session_id = session_id.filter(|_| backend == from);
        let paths = self.checkout_paths(&repo_path).await?;
        let sent = attached::prompt(&self.daemon, &threads, &text).await?;
        // The run's fields before it moves, to move it back if its new CLI doesn't start.
        let moved_from = self
            .store_options(prepared.resolved.backend(), changes)
            .await?;
        let message = text;
        let (prompt, resume) = if let Some(session_id) = session_id {
            info!(run = %self.id, "resuming an agent run's session");
            (sent, Some(self.resume_of(session_id).await?))
        } else {
            info!(run = %self.id, from, to = backend, "starting a new session for an agent run");
            match self
                .handoff_prompt(&from, &sent, &prepared.place, paths.as_ref())
                .await
            {
                Ok(prompt) => (prompt, None),
                Err(error) => {
                    self.move_back(moved_from).await;
                    return Err(error);
                }
            }
        };
        let fresh = resume.is_none();
        let to = backend.to_owned();
        // The old session, if any, is another CLI's, so the new CLI's start never stores it.
        let old_session = if fresh {
            self.row.state.session_id.take()
        } else {
            None
        };
        if self
            .launch(prepared, prompt, images, Some(turn_id), resume, paths)
            .await
        {
            if fresh {
                self.push(AgentOutputItem::Notice {
                    detail: handoff_notice(&from, &to),
                })
                .await;
            }
            // Only a turn that reached a CLI counts as sent: a retry after a failed start
            // tries again.
            self.record_turn(turn_id, message.clone()).await;
            self.attach(Some(turn_id), threads);
            self.last_message = message;
        } else {
            self.row.state.session_id = old_session;
            self.move_back(moved_from).await;
        }
        self.snapshot()
    }

    /// Stores `changes` to the run's options, checked against `backend`. When `backend` isn't the
    /// run's, moves the run to it, where another vendor's model can't carry over but the other
    /// options can if `backend` maps them, and returns the run's fields from before the move.
    async fn store_options(
        &mut self,
        backend: &dyn Backend,
        changes: RunOptions,
    ) -> Result<Option<parallax_store::RunFields>, ErrorObject> {
        let fields = &self.row.fields;
        let moving = backend.name() != fields.backend;
        let updated = if moving {
            let effort = fields.effort.as_deref().and_then(option_value);
            let permission = fields.permission.as_deref().and_then(option_value);
            let options = RunOptions {
                model: changes.model,
                effort: changes
                    .effort
                    .or(effort.filter(|effort| backend.efforts().contains(effort))),
                permission: changes
                    .permission
                    .or(permission.filter(|permission| backend.permissions().contains(permission))),
                context_window: changes.context_window.or(fields
                    .context_window
                    .filter(|tokens| backend.context_windows().contains(tokens))),
                fast: changes.fast.or(fields.fast.filter(|_| backend.fast_mode())),
            };
            options.check(backend)?;
            parallax_store::RunFields {
                backend: backend.name().to_owned(),
                model: options.model,
                effort: options.effort.and_then(option_name),
                permission: options.permission.and_then(option_name),
                context_window: options.context_window,
                fast: options.fast,
                ..fields.clone()
            }
        } else {
            if changes == RunOptions::default() {
                return Ok(None);
            }
            changes.check(backend)?;
            parallax_store::RunFields {
                model: changes.model.or(fields.model.clone()),
                effort: changes
                    .effort
                    .and_then(option_name)
                    .or(fields.effort.clone()),
                permission: changes
                    .permission
                    .and_then(option_name)
                    .or(fields.permission.clone()),
                context_window: changes.context_window.or(fields.context_window),
                fast: changes.fast.or(fields.fast),
                ..fields.clone()
            }
        };
        let id = self.row.id;
        let row = store(&self.daemon, move |db| {
            db.set_run_options(id, &updated)
                .map_err(|error| store_error(&error))
        })
        .await?;
        let before = std::mem::replace(&mut self.row.fields, row.fields);
        Ok(moving.then_some(before))
    }

    /// What a CLI needs to resume `session_id`: it, and the usage it has reported so far.
    async fn resume_of(&self, session_id: String) -> Result<Resume, ErrorObject> {
        let session = session_id.clone();
        let totals = store(&self.daemon, move |db| {
            db.session_usage_totals(&session)
                .map_err(|error| store_error(&error))
        })
        .await?;
        Ok(Resume {
            session_id,
            usage_totals: totals.into_iter().map(model_usage).collect(),
        })
    }

    /// Moves the run back to the backend and options it had, `fields`, after its move to another
    /// backend failed before a CLI started there.
    async fn move_back(&mut self, fields: Option<parallax_store::RunFields>) {
        let Some(fields) = fields else {
            return;
        };
        let id = self.row.id;
        let moved = store(&self.daemon, move |db| {
            db.set_run_options(id, &fields)
                .map_err(|error| store_error(&error))
        })
        .await;
        match moved {
            Ok(row) => {
                self.row.fields = row.fields;
                self.save().await;
            }
            Err(error) => {
                warn!(run = %self.id, error = %error.message, "could not move a run back to its backend");
            }
        }
    }

    /// The first message of a new session that takes over the run from one on `from`: what the
    /// run's first message says about where the agent is and what it may do, if anything, then
    /// the conversation so far, then `text`.
    async fn handoff_prompt(
        &mut self,
        from: &str,
        text: &str,
        place: &Place,
        paths: Option<&(PathBuf, PathBuf)>,
    ) -> Result<String, ErrorObject> {
        // What the agent said last is logged before the conversation is read.
        self.flush().await;
        let events = logged_events(&self.daemon, self.id).await?;
        let message = handoff_message(from, &conversation(&events, HISTORY_BYTES), text);
        match place {
            Place::Coordinator { repo } => Ok(super::coordinator::first_message(
                &message,
                &repo.to_string_lossy(),
            )),
            // A thread's first message is the user's own (0034).
            Place::Worker { thread: true, .. } => Ok(message),
            Place::Worker { context, .. } => {
                let cwd = match paths {
                    Some((cwd, _)) => cwd.clone(),
                    None => self.worker_paths().await?.0,
                };
                Ok(worker_prompt(&message, &cwd, context))
            }
        }
    }

    /// Records that `turn_id` was sent with `text`, in memory and in the store, so a retry of
    /// `agent/send` stays idempotent across a plxd restart, not only across a resumed CLI
    /// process within the same plxd (#190).
    /// Runs after the CLI has already accepted the turn (`live.run.send`'s `Ok`, or a successful
    /// `launch` in `resume`), so a crash between the two makes a retried `agent/send` after a
    /// restart send the message again: at-least-once, not exactly-once (#190 review non-blocking
    /// note). That's the same failure mode #190 was fixing in the other direction (a restart
    /// forgetting a turn was ever sent), and strictly better: a duplicate is visible in the
    /// transcript, a lost retry silently drops the user's message.
    async fn record_turn(&mut self, turn_id: TurnId, text: String) {
        self.turns.insert(turn_id, text.clone());
        let (run_id, id) = (self.row.id, self.id);
        let log = Arc::clone(&self.daemon.log);
        let stored = store(&self.daemon, move |db| {
            db.record_turn(run_id, turn_id.into(), &text)
                .map_err(|error| store_error(&error))?;
            crate::threads::prompted(db, &log, run_id)
        })
        .await;
        if let Err(error) = stored {
            warn!(run = %id, error = %error.message, "could not store a sent turn");
        }
    }

    /// Stores the images of `turn_id`'s message, which its CLI has now taken, for its
    /// `TurnStarted` to list (RYA-191, decision 0026). If they can't be stored, the CLI still has
    /// them, and the transcript shows the message without them.
    async fn keep_images(&mut self, turn_id: Option<TurnId>, images: Vec<PromptImage>) {
        if images.is_empty() {
            return;
        }
        let ids: Vec<ImageId> = images.iter().map(|_| ImageId::generate()).collect();
        let rows: Vec<_> = ids
            .iter()
            .zip(images)
            .map(|(&id, image)| {
                let stored = StoredImage {
                    media_type: option_name(image.media_type).unwrap_or_default(),
                    data: image.data,
                };
                (Uuid::from(id), stored)
            })
            .collect();
        let run = self.row.id;
        let stored = store(&self.daemon, move |db| {
            db.add_images(run, &rows)
                .map_err(|error| store_error(&error))
        })
        .await;
        match stored {
            Ok(()) => {
                self.images.insert(turn_id, ids);
            }
            Err(error) => {
                warn!(run = %self.id, error = %error.message, "could not store a message's images");
            }
        }
    }

    /// Keeps the threads attached to `turn_id`'s message, which its CLI has now taken, for its
    /// `TurnStarted` to list (PLX-372).
    pub(super) fn attach(&mut self, turn_id: Option<TurnId>, threads: Vec<RunId>) {
        if !threads.is_empty() {
            self.attached.insert(turn_id, threads);
        }
    }

    /// Starts the run's CLI with `prompt` and `images` and records the result: `running`, or
    /// `failed` with why. `paths` are a worker's worktree's and repository git folder's canonical
    /// paths, when the caller already has them. Returns whether the CLI started.
    pub async fn launch(
        &mut self,
        prepared: Prepared,
        prompt: String,
        images: Vec<PromptImage>,
        turn_id: Option<TurnId>,
        resume: Option<Resume>,
        paths: Option<(PathBuf, PathBuf)>,
    ) -> bool {
        let Prepared {
            resolved,
            accounts,
            place,
        } = prepared;
        let setup = match place {
            Place::Worker {
                home,
                data_dir,
                context,
                thread,
            } => {
                self.worker_setup(&home, &data_dir, &context, paths, thread)
                    .await
            }
            Place::Coordinator { repo } => self.coordinator_setup(repo),
        };
        let Setup {
            cwd,
            sandbox,
            temp,
            tools,
            thread,
        } = match setup {
            Ok(setup) => setup,
            Err(message) => {
                self.failed_to_start(message).await;
                return false;
            }
        };
        let account_id = resolved.account_id();
        let request = RunRequest {
            run_id: self.id,
            turn_id,
            cwd,
            prompt,
            images: images.clone(),
            policy: resolved.policy(),
            sandbox,
            account: AccountRef {
                id: account_id.clone(),
                credential: Credential::Subscription { config_home: None },
            },
            resume,
            model: self.row.fields.model.clone(),
            effort: self.row.fields.effort.as_deref().and_then(option_value),
            permission: self.row.fields.permission.as_deref().and_then(option_value),
            context_window: self.row.fields.context_window,
            fast: self.row.fields.fast,
            coordinator_tools: tools,
            approvals: self.row.fields.approvals,
            thread,
        };
        match routing::start(Arc::clone(&self.daemon.keys), &accounts, resolved, request) {
            Ok(started) => {
                self.live = Some(Live {
                    run: started.run,
                    events: started.events,
                    temp,
                });
                self.daemon.agents.running.fetch_add(1, Ordering::Relaxed);
                // The prompt's turn.
                self.in_flight = 1;
                self.held = false;
                convert::RUNNING.clone_into(&mut self.row.state.status);
                self.row.state.account_id = account_id;
                self.row.state.error = None;
                self.row.state.resume_at = None;
                self.save().await;
                self.keep_images(turn_id, images).await;
                true
            }
            Err(error) => {
                self.failed_to_start(error.to_string()).await;
                false
            }
        }
    }

    /// A worker's worktree and sandbox, with a new temp folder for its CLI.
    async fn worker_setup(
        &self,
        home: &Path,
        data_dir: &Path,
        context: &Path,
        paths: Option<(PathBuf, PathBuf)>,
        thread: bool,
    ) -> Result<Setup, String> {
        let (cwd, git_common_dir) = match paths {
            Some(paths) => paths,
            None => self.worker_paths().await.map_err(|error| error.message)?,
        };
        let (temp, temp_path) = self.run_temp().map_err(|error| error.message)?;
        let sandbox =
            WorkerSandbox::for_worktree(home, data_dir, &cwd, &git_common_dir, context, &temp_path);
        Ok(Setup {
            cwd,
            sandbox: Some(sandbox),
            temp: Some(temp),
            tools: None,
            thread,
        })
    }

    /// A coordinator runs in the project's repository (0027), with its Parallax tools, bound to its
    /// project and to its own thread (0019).
    fn coordinator_setup(&mut self, repo: PathBuf) -> Result<Setup, String> {
        let program = std::env::current_exe()
            .map_err(|error| format!("could not find plxd's own executable: {error}"))?;
        let thread = self
            .row
            .fields
            .coordinator_thread
            .and_then(|id| CoordinatorThreadId::try_from(id).ok())
            .ok_or_else(|| format!("coordinator run {} has no thread id", self.id))?;
        let tools = CoordinatorTools {
            program,
            data_dir: self.daemon.data_dir.root().to_owned(),
            project: self.project,
            thread,
        };
        Ok(Setup {
            cwd: repo,
            sandbox: None,
            temp: None,
            tools: Some(tools),
            thread: false,
        })
    }

    /// A new temp folder for the run's CLI (RYA-130), a resumed run's too, and its canonical
    /// path for the sandbox.
    fn run_temp(&self) -> Result<(RunTemp, PathBuf), ErrorObject> {
        let temp = run_temp::create(&self.daemon.data_dir).map_err(|error| {
            worker_unavailable(format!("could not make the run's temp folder: {error}"))
        })?;
        let path = sandbox_path(temp.path(), "the run's temp folder")?;
        Ok((temp, path))
    }

    /// For a thread in its repository's own checkout, the paths it starts in: `repo_path`'s.
    /// `None` for any other run, whose worktree [`Self::worker_paths`] finds.
    async fn checkout_paths(
        &self,
        repo_path: &str,
    ) -> Result<Option<(PathBuf, PathBuf)>, ErrorObject> {
        if !self.row.fields.checkout {
            return Ok(None);
        }
        super::checkout_paths(&self.daemon.agents, Path::new(repo_path))
            .await
            .map(Some)
    }

    async fn worker_paths(&self) -> Result<(PathBuf, PathBuf), ErrorObject> {
        let Some(worktree) = &self.worktree else {
            return Err(super::run_accepted(self.id));
        };
        let cwd = sandbox_path(Path::new(&worktree.path), "the run's worktree")?;
        let git_dir = self
            .daemon
            .agents
            .worktrees
            .git_common_dir(Path::new(&worktree.repo_path))
            .await
            .map_err(|error| ErrorObject::parallax(ErrorKind::WorktreeFailed, error.to_string()))?;
        let git_dir = sandbox_path(&git_dir, "the repository's git folder")?;
        Ok((cwd, git_dir))
    }

    async fn failed_to_start(&mut self, message: String) {
        warn!(run = %self.id, %message, "an agent run's CLI could not start");
        self.append(ParallaxEvent::AgentFinished {
            run_id: self.id,
            outcome: AgentOutcome::Failed {
                failure: AgentFailureKind::SpawnFailed,
                message: message.clone(),
            },
        })
        .await;
        convert::FAILED.clone_into(&mut self.row.state.status);
        self.row.state.error = Some(message);
        self.row.state.resume_at = None;
        self.save().await;
    }

    async fn on_event(&mut self, event: Option<Event>) {
        let Some(event) = event else {
            // An `EventStream` always ends with `Finished`, which clears `live` first.
            self.clear_live();
            return;
        };
        match &event {
            Event::SessionStarted { session_id, .. } => {
                if let Some(item) = output_item(&event) {
                    self.push(item).await;
                }
                self.row.state.session_id = Some(session_id.clone());
                self.save().await;
            }
            Event::AccountFallback {
                from_account,
                to_account,
                reason,
            } => {
                self.flush().await;
                self.append(ParallaxEvent::AgentAccountFallback {
                    run_id: self.id,
                    from_account: from_account.clone(),
                    to_account: to_account.clone(),
                    reason: convert::failure_kind(*reason),
                })
                .await;
                self.row.state.account_id.clone_from(to_account);
                // The first account's limits don't bind the account the run moved to.
                self.resumes.take_reset();
                self.save().await;
            }
            Event::Usage(_) | Event::RateLimit(_) => {
                if let Event::RateLimit(window) = &event {
                    self.resumes.saw(window);
                }
                self.record_usage(event.clone()).await;
                if let Some(item) = output_item(&event) {
                    self.push(item).await;
                }
            }
            Event::ApprovalRequested(request) => {
                let timeout = self.daemon.agents.approval_timeout();
                let offers_always = !request.always_allow.is_empty();
                let deadline = Instant::now() + timeout;
                self.approvals
                    .add(request.approval_id, deadline, offers_always, &request.input);
                let expires_at = jiff::SignedDuration::try_from(timeout)
                    .ok()
                    .and_then(|timeout| jiff::Timestamp::now().checked_add(timeout).ok())
                    .unwrap_or(jiff::Timestamp::MAX);
                self.push(convert::approval_requested(request, expires_at))
                    .await;
            }
            Event::ApprovalWithdrawn { approval_id } => {
                let withdrawn = ended(AgentApprovalDecision::Withdrawn, AgentApprovalBy::Agent);
                self.resolve_approval(*approval_id, withdrawn).await;
            }
            Event::Finished { outcome, .. } => {
                let outcome = outcome.clone();
                self.record_usage(event).await;
                self.clear_live();
                // The CLI exited while they waited, so nothing can answer them now.
                for approval_id in self.approvals.pending() {
                    let gone = ended(AgentApprovalDecision::Withdrawn, AgentApprovalBy::Agent);
                    self.resolve_approval(approval_id, gone).await;
                }
                self.finish(&outcome).await;
            }
            _ => {
                if self.track_turns(&event).await {
                    return;
                }
                let created = self.created_prs(&event);
                if let Some(mut item) = output_item(&event) {
                    // A follow-up's text, which `send` recorded before its CLI could report the
                    // turn, so a transcript rebuilt from the log shows it (RYA-92), capped like
                    // every other text item, the ids of any message's images (RYA-191), and the
                    // threads attached to it (PLX-372).
                    if let AgentOutputItem::TurnStarted {
                        turn_id,
                        text,
                        wake,
                        images,
                        threads,
                    } = &mut item
                    {
                        *images = self.images.remove(turn_id).unwrap_or_default();
                        *threads = self.attached.remove(turn_id).unwrap_or_default();
                        if let Some(turn_id) = turn_id {
                            // A coordinator's wake-up or a usage limit's resume: plxd's own turn.
                            *wake =
                                self.wakes.was_sent(*turn_id) || self.resumes.was_sent(*turn_id);
                            *text = self
                                .turns
                                .get(turn_id)
                                .map(|sent| convert::truncate(sent, convert::MAX_TEXT_ITEM_BYTES));
                        }
                    }
                    self.push(item).await;
                }
                for url in created {
                    self.link_pr(url).await;
                }
            }
        }
    }

    /// Counts the live CLI's turns in flight, and puts a message it took but drops as it exits
    /// back first in the queue, for the next CLI (PLX-370). Returns whether that happened, so the
    /// message isn't logged dropped.
    async fn track_turns(&mut self, event: &Event) -> bool {
        if matches!(
            event,
            Event::TurnFinished { .. } | Event::FollowUpDropped { .. }
        ) {
            self.in_flight = self.in_flight.saturating_sub(1);
        }
        match event {
            Event::TurnStarted {
                turn_id: Some(turn_id),
            } => self.handed.retain(|queued| queued.turn_id != *turn_id),
            Event::FollowUpDropped { turn_id } => {
                if let Some(at) = self.handed.iter().position(|q| q.turn_id == *turn_id) {
                    let queued = self.handed.remove(at);
                    self.queued.push_front(queued);
                    self.save_queue().await;
                    return true;
                }
            }
            _ => {}
        }
        false
    }

    fn clear_live(&mut self) {
        self.in_flight = 0;
        self.held = false;
        self.handed.clear();
        if let Some(live) = self.live.take() {
            self.daemon.agents.running.fetch_sub(1, Ordering::Relaxed);
            // A worker's temp can hold a whole package store, so it goes off this task's thread.
            tokio::task::spawn_blocking(move || drop(live.temp));
        }
    }

    async fn record_usage(&self, event: Event) {
        let session = self.row.state.session_id.clone();
        if matches!(event, Event::Finished { .. }) && session.is_none() {
            return;
        }
        let (id, account) = (self.id, self.row.state.account_id.clone());
        let recorded = store(&self.daemon, move |db| {
            crate::usage::record_event(db, id, &account, session.as_deref().unwrap_or(""), &event)
                .map_err(|error| store_error(&error))
        })
        .await;
        if let Err(error) = recorded {
            warn!(run = %self.id, error = %error.message, "could not record an agent run's usage");
        }
    }

    /// Records how a CLI process ended. Unless plxd stopped it, commits a worker's changes first,
    /// through #166's hardened commit, and reports the commit, then wakes the coordinator that
    /// started the run.
    async fn finish(&mut self, outcome: &Outcome) {
        self.flush().await;
        if self.stopping && matches!(outcome, Outcome::Cancelled) {
            info!(run = %self.id, "an agent run was interrupted because plxd is stopping");
            self.append(ParallaxEvent::AgentFinished {
                run_id: self.id,
                outcome: AgentOutcome::Interrupted,
            })
            .await;
            convert::INTERRUPTED.clone_into(&mut self.row.state.status);
            self.save().await;
            return;
        }
        let (mut outcome, mut status, mut error) = convert::outcome(outcome);
        // A coordinator's worktree and a thread's checkout are never committed.
        let committed = if self.is_coordinator() || self.row.fields.checkout {
            Ok(None)
        } else {
            self.commit(&commit_message(&self.last_message, self.id))
                .await
        };
        let diff = match committed {
            Ok(diff) => diff,
            Err(message) => {
                warn!(run = %self.id, %message, "could not commit an agent run's changes");
                if matches!(outcome, AgentOutcome::Completed { .. }) {
                    outcome = AgentOutcome::Failed {
                        failure: AgentFailureKind::CommitFailed,
                        message: message.clone(),
                    };
                }
                status = convert::FAILED;
                error = Some(message);
                None
            }
        };
        self.append(ParallaxEvent::AgentFinished {
            run_id: self.id,
            outcome: outcome.clone(),
        })
        .await;
        if let Some(diff) = diff {
            self.record_diff(diff).await;
        }
        status.clone_into(&mut self.row.state.status);
        self.row.state.error = error;
        self.after_limit(&outcome).await;
        info!(run = %self.id, status = %self.row.state.status, "an agent run's CLI finished");
        self.save().await;
        if let Some(thread) = self.row.fields.coordinator_thread
            && !self.is_coordinator()
            && let Ok(run) = self.snapshot()
        {
            wake::notify(&self.daemon, thread, wake::summary(&run, &outcome));
        }
    }

    /// Records the run's new commit and its diff, and tells clients, as `agent.diffReady`. The
    /// caller saves the row.
    async fn record_diff(&mut self, diff: DiffSummary) {
        self.row.state.commit_sha = Some(diff.commit.clone());
        self.row.state.files_changed = Some(diff.files);
        self.row.state.insertions = Some(diff.insertions);
        self.row.state.deletions = Some(diff.deletions);
        self.append(ParallaxEvent::AgentDiffReady {
            run_id: self.id,
            diff,
        })
        .await;
    }

    /// Commits whatever the run changed in its worktree, on its branch, with `message`, and
    /// measures the branch against the worktree's base. `None` when there was nothing new to
    /// commit.
    async fn commit(&self, message: &str) -> Result<Option<DiffSummary>, String> {
        let Some(worktree) = &self.worktree else {
            return Err("the run was accepted, and its worktree is gone".to_owned());
        };
        if worktree.git_dir.is_empty() {
            return Err(
                "the run's worktree has no recorded git folder, so plxd can't commit it safely"
                    .to_owned(),
            );
        }
        let worktrees = &self.daemon.agents.worktrees;
        let path = Path::new(&worktree.path);
        let git_dir = Path::new(&worktree.git_dir);
        let commit = worktrees
            .commit_all(path, git_dir, Path::new(&worktree.repo_path), message)
            .await
            .map_err(|error| error.to_string())?;
        let Some(commit) = commit else {
            return Ok(None);
        };
        let stat = worktrees
            .diff_stat(path, git_dir, &worktree.base)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Some(DiffSummary {
            commit: commit.sha,
            files: stat.files,
            insertions: stat.insertions,
            deletions: stat.deletions,
        }))
    }

    async fn push(&mut self, item: AgentOutputItem) {
        self.batch.bytes += item_bytes(&item);
        self.batch.items.push(item);
        self.batch.since.get_or_insert_with(Instant::now);
        if self.batch.bytes >= MAX_BATCH_BYTES {
            self.flush().await;
        }
    }

    async fn flush(&mut self) {
        let batch = std::mem::take(&mut self.batch);
        if !batch.items.is_empty() {
            self.append(ParallaxEvent::AgentOutput {
                run_id: self.id,
                items: batch.items,
            })
            .await;
        }
    }

    /// From a tokio task: the event log's own writer thread does the SQLite work, so awaiting it
    /// here yields this actor's worker thread to other work instead of blocking it (#190).
    async fn append(&self, event: ParallaxEvent) -> u64 {
        self.daemon
            .log
            .append(jiff::Timestamp::now(), Some(self.project), event)
            .await
    }

    /// Stores the run's state and reports it as `agent.updated`, after any transcript items
    /// waiting to be sent.
    async fn save(&mut self) {
        self.flush().await;
        let (id, state) = (self.row.id, self.row.state.clone());
        let saved = store(&self.daemon, move |db| {
            db.update_run(id, &state)
                .map_err(|error| store_error(&error))
        })
        .await;
        match saved {
            Ok(row) => self.row = row,
            Err(error) => {
                warn!(run = %self.id, error = %error.message, "could not store an agent run's state");
            }
        }
        self.append(ParallaxEvent::AgentUpdated {
            run_id: self.id,
            state: convert::run_state(&self.row),
        })
        .await;
    }
}

fn id_conflict(turn_id: TurnId) -> ErrorObject {
    ErrorObject::parallax(
        ErrorKind::IdConflict,
        format!("turn {turn_id} was already sent with a different text"),
    )
}

/// The account a run's session belongs to, as routing takes it: a key account's id, or else a
/// backend's name for its subscription (0012).
fn session_account(account_id: &str) -> AccountChoice {
    match account_id.parse::<AccountId>() {
        Ok(id) => AccountChoice::Key { id },
        Err(_) => AccountChoice::Subscription {
            backend: account_id.to_owned(),
        },
    }
}

/// About the most of the conversation a new session is told, in bytes: its latest messages.
const HISTORY_BYTES: usize = 64 * 1024;

/// What a [`conversation`] cut to its cap starts with. One that isn't cut starts with who spoke.
pub(super) const LEFT_OUT: &str = "(Earlier messages are left out.)\n\n";

/// Every event run `run` logged, oldest first.
async fn logged_events(daemon: &Daemon, run: RunId) -> Result<Vec<ParallaxEvent>, ErrorObject> {
    let log = Arc::clone(&daemon.log);
    tokio::task::spawn_blocking(move || {
        let mut events = Vec::new();
        let mut after = 0;
        loop {
            let (page, more) = log.run_events(run, after, 1000, 4 * 1024 * 1024)?;
            after = page.last().map_or(after, |entry| entry.seq);
            events.extend(page.iter().map(|entry| entry.event.clone()));
            if !more || page.is_empty() {
                return Ok(events);
            }
        }
    })
    .await
    .map_err(ErrorObject::internal_error)?
    .map_err(|error| store_error(&error))
}

/// A run's conversation as `events` logged it, for a new session to take over (0014) or a
/// message it's attached to (PLX-372): the user's messages, Parallax's wake-ups, and the agent's
/// replies, oldest first, without its tool calls, whose work is in the run's folder. Only the
/// latest that fit in about `cap` bytes are kept.
pub(super) fn conversation(events: &[ParallaxEvent], cap: usize) -> String {
    let mut said: Vec<String> = Vec::new();
    // The last thing the agent said, which a turn's result often repeats.
    let mut last_reply = String::new();
    for event in events {
        match event {
            ParallaxEvent::AgentStarted { run: Some(run), .. } => {
                said.push(format!("User:\n{}", run.prompt.trim()));
            }
            ParallaxEvent::AgentOutput { items, .. } => {
                for item in items {
                    match item {
                        AgentOutputItem::TurnStarted {
                            text: Some(text),
                            wake,
                            ..
                        } => {
                            let who = if *wake { "Parallax" } else { "User" };
                            said.push(format!("{who}:\n{}", text.trim()));
                        }
                        AgentOutputItem::Text { text, .. } if !text.trim().is_empty() => {
                            text.trim().clone_into(&mut last_reply);
                            said.push(format!("Agent:\n{last_reply}"));
                        }
                        AgentOutputItem::TurnFinished {
                            result: Some(result),
                            ..
                        } if !result.trim().is_empty() && result.trim() != last_reply => {
                            result.trim().clone_into(&mut last_reply);
                            said.push(format!("Agent:\n{last_reply}"));
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    let mut kept = Vec::new();
    let mut bytes = 0;
    for message in said.iter().rev() {
        if bytes + message.len() > cap {
            // One message longer than the whole is cut, rather than leaving nothing.
            if kept.is_empty() {
                kept.push(convert::truncate(message, cap));
            }
            break;
        }
        bytes += message.len();
        kept.push(message.clone());
    }
    let cut = kept.len() < said.len();
    kept.reverse();
    let conversation = kept.join("\n\n");
    if cut {
        format!("{LEFT_OUT}{conversation}")
    } else {
        conversation
    }
}

/// The message a new session on another backend gets in place of the user's `message`: that it
/// takes over from an agent on `from`, and what was said so far.
fn handoff_message(from: &str, conversation: &str, message: &str) -> String {
    format!(
        "This conversation began with another agent, on {from}, and the user has handed it to \
         you. Its work so far is in your working folder. Here is the conversation, oldest \
         first:\n\n<conversation>\n{conversation}\n</conversation>\n\nThe user's new message, \
         which is yours to answer:\n{message}",
        from = backend_name(from),
    )
}

/// The transcript's line where a new session took over the run from one on `from`.
fn handoff_notice(from: &str, to: &str) -> String {
    if from == to {
        "A new session picks up this conversation from what was said so far.".to_owned()
    } else {
        format!(
            "Moved from {} to {}: a new session picks up this conversation from what was said \
             so far.",
            backend_name(from),
            backend_name(to),
        )
    }
}

/// A backend's name for people.
fn backend_name(backend: &str) -> &str {
    match backend {
        "claude" => "Claude Code",
        "codex" => "Codex",
        "cursor" => "Cursor",
        other => other,
    }
}

async fn next_event(live: &mut Option<Live>) -> Option<Event> {
    match live {
        Some(live) => live.events.next().await,
        None => std::future::pending().await,
    }
}

fn model_usage(total: SessionModelUsage) -> ModelUsage {
    ModelUsage {
        model: total.model,
        usage: Usage {
            input_tokens: total.input_tokens,
            output_tokens: total.output_tokens,
            cache_read_tokens: total.cache_read_tokens,
            cache_write_tokens: total.cache_write_tokens,
            cost_usd_micros: total.cost_usd_micros,
        },
    }
}

/// The merge commit's message, when accepting a run needs one: `Merge Parallax run: <the task's first
/// line>`, cut to 72 characters, then the run and its branch.
fn merge_message(prompt: &str, run: RunId, branch: &str) -> String {
    let first = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("agent run");
    let mut subject: String = format!("Merge Parallax run: {}", first.trim());
    if subject.chars().count() > 72 {
        subject = subject.chars().take(69).collect::<String>() + "...";
    }
    format!("{subject}\n\nAccepted in parallax: agent run {run}, branch {branch}.\n")
}

fn accept_error(error: &crate::worktree::AcceptError) -> ErrorObject {
    use crate::worktree::AcceptError;
    match error {
        AcceptError::Refused(message) => ErrorObject::parallax(ErrorKind::MergeRefused, message),
        AcceptError::Conflict { .. } => {
            ErrorObject::parallax(ErrorKind::MergeConflict, error.to_string())
        }
        AcceptError::Git(error) => {
            ErrorObject::parallax(ErrorKind::MergeRefused, error.to_string())
        }
    }
}

/// `parallax: <the message's first line>`, cut to 72 characters, then the run it belongs to.
fn commit_message(message: &str, run: RunId) -> String {
    let first = message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("agent run");
    let mut subject: String = format!("parallax: {}", first.trim());
    if subject.chars().count() > 72 {
        subject = subject.chars().take(69).collect::<String>() + "...";
    }
    format!("{subject}\n\nCommitted by Parallax for agent run {run}.\n")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use parallax_protocol::{
        AccountChoice, AccountId, AgentApprovalAnswer, AgentApprovalBy, AgentApprovalDecision,
        AgentApproveParams, AgentOutputItem, ApprovalId, ErrorKind, ParallaxEvent, ProjectId,
        RunId, TurnId,
    };
    use parallax_store::{Run as RunRow, RunFields, RunState, Worktree};
    use tokio::sync::{mpsc, oneshot};
    use tokio_util::sync::CancellationToken;

    use super::{
        Actor, Command, HISTORY_BYTES, Live, Queued, attached, commit_message, conversation,
        handoff_message, handoff_notice, session_account,
    };
    use crate::agents::RunOptions;
    use crate::agents::convert::agent_run;
    use crate::backend::{
        Answer, AnswerError, ApprovalRequest, Event, EventSink, FollowUp, Run, SendError,
    };
    use crate::server::Daemon;

    /// A message for [`Actor::send`].
    fn message(turn_id: TurnId, text: &str, options: RunOptions) -> Queued {
        Queued {
            turn_id,
            text: text.to_owned(),
            images: Vec::new(),
            threads: Vec::new(),
            options,
            account: None,
        }
    }

    #[test]
    fn a_session_resumes_on_the_account_it_ended_on() {
        let key = AccountId::generate();
        assert_eq!(
            session_account(&key.to_string()),
            AccountChoice::Key { id: key },
            "after a fallback, the key account"
        );
        assert_eq!(
            session_account("claude"),
            AccountChoice::Subscription {
                backend: "claude".to_owned()
            }
        );
    }

    #[test]
    fn commit_messages_are_one_short_subject_and_the_run() {
        let run = RunId::generate();
        let message = commit_message("\n  Add a README\nwith details", run);
        assert!(
            message.starts_with("parallax: Add a README\n\n"),
            "{message}"
        );
        assert!(message.contains(&run.to_string()));
        let long = commit_message(&"x".repeat(200), run);
        assert_eq!(long.lines().next().unwrap().chars().count(), 72);
        assert!(commit_message("", run).starts_with("parallax: agent run"));
    }

    struct NoopRun;

    impl Run for NoopRun {
        fn id(&self) -> RunId {
            RunId::generate()
        }

        fn send(&self, _: FollowUp) -> Result<(), SendError> {
            Err(SendError::Unsupported)
        }

        fn cancel(&self) {}
    }

    /// A run row and worktree that never touch the store: enough for a `Command::Cancel`, which
    /// only signals `live.run` and replies with a snapshot.
    fn fake_row_and_worktree() -> (RunRow, Worktree) {
        let now = jiff::Timestamp::now();
        let id = uuid::Uuid::from(RunId::generate());
        let row = RunRow {
            id,
            fields: RunFields {
                project_id: ProjectId::generate().into(),
                prompt: "flood".to_owned(),
                requested_account: None,
                policy: "workspaceWrite".to_owned(),
                backend: "fake".to_owned(),
                coordinator_thread: None,
                parent: None,
                model: None,
                effort: None,
                permission: None,
                context_window: None,
                fast: None,
                approvals: false,
                checkout: false,
            },
            state: RunState {
                status: "running".to_owned(),
                account_id: "fake".to_owned(),
                ..RunState::default()
            },
            created_at: now,
            updated_at: now,
        };
        let worktree = Worktree {
            id,
            repo_path: "/tmp".to_owned(),
            path: "/tmp".to_owned(),
            branch: "parallax/run".to_owned(),
            base: "0".repeat(40),
            git_dir: String::new(),
            base_dirty: false,
            created_at: now,
        };
        (row, worktree)
    }

    /// #190 N6 / review item 5: `Actor::run`'s select order lets a queued `agent/cancel` through
    /// promptly even while the backend keeps producing output, instead of only once its stream
    /// goes quiet. Deterministic, on a `current_thread` runtime: the whole flood is buffered in
    /// the channel *before* the actor's loop ever runs, so the event branch of its `select!` is
    /// synchronously ready on every iteration without needing a producer task to keep pace with
    /// the consumer — nothing here depends on real concurrency or timing. `Command::Cancel` is
    /// likewise queued before the loop starts, so on its very first iteration both branches are
    /// ready and only the `select!`'s order decides which one runs. Under the old, event-first
    /// order this drains the whole flood — appending it as `agent.output` — before ever reaching
    /// the command; confirmed by temporarily restoring that order and observing this test fail on
    /// the `head()` assertion below, well past the timeout.
    #[tokio::test]
    async fn a_cancel_is_answered_promptly_while_output_floods_in() {
        const FLOOD: usize = 10_000;
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::for_tests(dir.path(), 100_000, Duration::from_secs(90));
        let (row, worktree) = fake_row_and_worktree();
        let mut actor = Actor::new(Arc::clone(&daemon), row, Some(worktree), HashMap::new());

        let (mut sink, events) = EventSink::channel(FLOOD, Vec::new());
        for _ in 0..FLOOD {
            sink.emit(Event::Text {
                message_id: None,
                text: "x".repeat(16),
            })
            .await
            .expect("the channel holds the whole flood");
        }
        actor.live = Some(Live {
            run: Arc::new(NoopRun),
            events,
            temp: Some(crate::backend::run_temp::create(&daemon.data_dir).unwrap()),
        });

        let (commands, receiver) = mpsc::channel(4);
        let (reply, answer) = oneshot::channel();
        commands
            .send(Command::Cancel { reply })
            .await
            .expect("the actor's command channel is open");

        let run_task = tokio::spawn(actor.run(receiver, CancellationToken::new()));

        tokio::time::timeout(Duration::from_secs(5), answer)
            .await
            .expect("a cancel command was never answered while output flooded in")
            .expect("the actor answered")
            .expect("cancelling a live run always succeeds");

        // Nothing but the one `Cancel` command has been processed: no event, and so nothing
        // appended to the log. The old order would have drained (and appended) some or all of
        // the 10,000-item flood by now.
        assert_eq!(
            daemon.log.head(),
            0,
            "the cancel was answered only after events were appended, not before"
        );

        drop(sink);
        drop(commands);
        run_task.abort();
    }

    /// The first attempt of a run whose account fell back (#119): its CLI exited, so it takes no
    /// more answers.
    struct ExitedRun;

    impl Run for ExitedRun {
        fn id(&self) -> RunId {
            RunId::generate()
        }

        fn send(&self, _: FollowUp) -> Result<(), SendError> {
            Err(SendError::Unsupported)
        }

        fn cancel(&self) {}

        fn answer(&self, _: Answer) -> Result<(), AnswerError> {
            Err(AnswerError::Finished)
        }
    }

    /// RYA-222: a request still pending when its attempt ended, answered while a fallback attempt
    /// runs in its place, is withdrawn at once. Waiting for the run's end would hold up the actor,
    /// and every command and expiry with it, for the fallback attempt's whole run.
    #[tokio::test]
    async fn an_answer_to_a_request_whose_attempt_ended_resolves_it_while_a_fallback_runs() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::for_tests(dir.path(), 100_000, Duration::from_secs(90));
        let (row, worktree) = fake_row_and_worktree();
        let mut actor = Actor::new(Arc::clone(&daemon), row, Some(worktree), HashMap::new());
        // The fallback attempt's events: open and quiet for as long as the test runs.
        let (_fallback, events) = EventSink::channel(4, Vec::new());
        actor.live = Some(Live {
            run: Arc::new(ExitedRun),
            events,
            temp: None,
        });
        let approval_id = ApprovalId::generate();
        let request = ApprovalRequest {
            approval_id,
            tool_name: "Bash".to_owned(),
            input: serde_json::json!({"command": "pnpm test"}),
            call_id: None,
            reason: None,
            blocked_path: None,
            subagent: None,
            always_allow: Vec::new(),
            interactive: false,
        };
        actor
            .on_event(Some(Event::ApprovalRequested(request)))
            .await;

        let allow = AgentApproveParams {
            run_id: actor.id,
            approval_id,
            decision: AgentApprovalAnswer::Allow,
            input: None,
            always: false,
            message: None,
        };
        let resolved = tokio::time::timeout(Duration::from_secs(5), actor.approve(allow))
            .await
            .expect("the answer waited for the fallback attempt to end")
            .unwrap();
        assert_eq!(resolved.decision, AgentApprovalDecision::Withdrawn);
        assert_eq!(resolved.by, AgentApprovalBy::Agent);
    }

    /// A run's turns as a new session is told them: the user's messages, Parallax's wake-ups,
    /// and the agent's replies, with a turn's result only where it adds to what the agent said.
    #[test]
    fn a_new_session_is_told_the_conversation_so_far() {
        let (row, worktree) = fake_row_and_worktree();
        let run = agent_run(&row, Some(&worktree)).unwrap();
        let output = |items| ParallaxEvent::AgentOutput {
            run_id: run.id,
            items,
        };
        let turn = |text: &str, wake| AgentOutputItem::TurnStarted {
            turn_id: Some(TurnId::generate()),
            text: Some(text.to_owned()),
            wake,
            images: Vec::new(),
            threads: Vec::new(),
        };
        let finished = |result: &str| AgentOutputItem::TurnFinished {
            turn_id: None,
            result: Some(result.to_owned()),
        };
        let events = [
            ParallaxEvent::AgentStarted {
                run_id: run.id,
                run: Some(run.clone()),
            },
            output(vec![
                AgentOutputItem::TurnStarted {
                    turn_id: None,
                    text: None,
                    wake: false,
                    images: Vec::new(),
                    threads: Vec::new(),
                },
                AgentOutputItem::ToolCall {
                    call_id: "call-1".to_owned(),
                    name: "Read".to_owned(),
                    input: serde_json::json!({"file_path": "README.md"}),
                },
                AgentOutputItem::Text {
                    message_id: None,
                    text: "Read it.\n".to_owned(),
                },
                finished("Read it."),
            ]),
            output(vec![turn(" And now? ", false), finished("All done.")]),
            output(vec![turn("Subagents finished", true)]),
        ];
        assert_eq!(
            conversation(&events, HISTORY_BYTES),
            "User:\nflood\n\nAgent:\nRead it.\n\nUser:\nAnd now?\n\nAgent:\nAll done.\n\n\
             Parallax:\nSubagents finished"
        );

        // A long one keeps its latest messages.
        let long: Vec<_> = (0..20)
            .map(|i| output(vec![turn(&format!("{i}{}", "x".repeat(8 * 1024)), false)]))
            .collect();
        let kept = conversation(&long, HISTORY_BYTES);
        assert!(kept.starts_with("(Earlier messages are left out.)\n\nUser:\n"));
        assert!(kept.ends_with(&format!("19{}", "x".repeat(8 * 1024))));
        assert!(kept.len() <= HISTORY_BYTES + 64, "{}", kept.len());
        // An attached thread's summary is cut the same way, to its own cap (PLX-372).
        let summary = conversation(&long, attached::SUMMARY_BYTES);
        assert!(summary.starts_with("(Earlier messages are left out.)\n\nUser:\n"));
        assert!(summary.ends_with(&format!("19{}", "x".repeat(8 * 1024))));
        assert!(
            summary.len() <= attached::SUMMARY_BYTES + 64,
            "{}",
            summary.len()
        );
        assert!(summary.len() < kept.len());
    }

    #[test]
    fn a_handoff_names_where_the_conversation_began() {
        let message = handoff_message("claude", "User:\nHi", "Carry on");
        assert!(
            message.contains("another agent, on Claude Code"),
            "{message}"
        );
        assert!(message.contains("<conversation>\nUser:\nHi\n</conversation>"));
        assert!(message.ends_with("The user's new message, which is yours to answer:\nCarry on"));
        assert_eq!(
            handoff_notice("claude", "codex"),
            "Moved from Claude Code to Codex: a new session picks up this conversation from what \
             was said so far."
        );
    }

    /// A message that changes the model can't reach a running CLI, so it waits, as does every
    /// message after it, a retry of it is the same message, and Stop drops them all.
    #[tokio::test]
    async fn messages_wait_for_a_running_cli_and_stop_drops_them() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::for_tests(dir.path(), 100_000, Duration::from_secs(90));
        let (row, worktree) = fake_row_and_worktree();
        let mut actor = Actor::new(Arc::clone(&daemon), row, Some(worktree), HashMap::new());
        let (_sink, events) = EventSink::channel(4, Vec::new());
        actor.live = Some(Live {
            run: Arc::new(NoopRun),
            events,
            temp: None,
        });

        let sonnet = RunOptions {
            model: Some("sonnet".to_owned()),
            ..RunOptions::default()
        };
        let (first, second) = (TurnId::generate(), TurnId::generate());
        let run = actor
            .send(message(first, "Hurry up", sonnet.clone()), false)
            .await
            .unwrap();
        assert_eq!(run.model, None, "nothing changes until the CLI exits");
        actor
            .send(message(second, "And then", RunOptions::default()), false)
            .await
            .unwrap();
        actor
            .send(message(first, "Hurry up", sonnet.clone()), false)
            .await
            .unwrap();
        let conflict = actor
            .send(message(first, "Other", sonnet), false)
            .await
            .unwrap_err();
        assert_eq!(
            conflict.parallax_data().unwrap().kind,
            ErrorKind::IdConflict
        );
        let waiting: Vec<_> = actor.queued.iter().map(|queued| queued.turn_id).collect();
        assert_eq!(waiting, [first, second]);

        let (reply, answer) = oneshot::channel();
        actor.on_command(Command::Cancel { reply }).await;
        answer.await.unwrap().unwrap();
        assert!(actor.queued.is_empty());
        actor.flush().await;
        let (logged, _) = daemon.log.run_events(actor.id, 0, 100, usize::MAX).unwrap();
        let dropped: Vec<_> = logged
            .iter()
            .filter_map(|entry| match &entry.event {
                ParallaxEvent::AgentOutput { items, .. } => Some(items),
                _ => None,
            })
            .flatten()
            .filter_map(|item| match item {
                AgentOutputItem::FollowUpDropped { turn_id } => Some(*turn_id),
                _ => None,
            })
            .collect();
        assert_eq!(dropped, [first, second]);
    }

    /// A backend that takes no messages while it runs gets one once its CLI exits, rather than
    /// refusing it.
    #[tokio::test]
    async fn a_message_a_running_cli_cant_take_waits_for_it() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::for_tests(dir.path(), 100_000, Duration::from_secs(90));
        let (row, worktree) = fake_row_and_worktree();
        let mut actor = Actor::new(Arc::clone(&daemon), row, Some(worktree), HashMap::new());
        let (_sink, events) = EventSink::channel(4, Vec::new());
        actor.live = Some(Live {
            run: Arc::new(NoopRun),
            events,
            temp: None,
        });
        let turn = TurnId::generate();
        actor
            .send(message(turn, "Also this", RunOptions::default()), false)
            .await
            .unwrap();
        assert_eq!(
            actor.queued.front().map(|queued| queued.turn_id),
            Some(turn)
        );
    }
}
