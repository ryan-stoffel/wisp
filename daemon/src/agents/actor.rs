//! One run's actor: the task that owns a run for as long as wispd runs.
//!
//! It takes commands (`agent/send`, `agent/cancel`) and the run's backend events in one loop, so
//! nothing about a run needs a lock, and events are logged in the order they happened.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio::time::{Instant, sleep_until};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{AcceptId, AgentMerge};
use wisp_protocol::{
    AccountChoice, AccountId, AgentFailureKind, AgentOutcome, AgentOutputItem, AgentRun,
    DiffSummary, ErrorKind, ProjectId, RunId, TurnId, WispEvent,
};
use wisp_store::{Run as RunRow, RunAccept, SessionModelUsage, Worktree};

use super::convert::{self, agent_run, item_bytes, output_item};
use super::worker::sandbox_path;
use super::{Prepared, prepare, store, store_error};
use crate::backend::{
    AccountRef, Credential, Event, EventStream, FollowUp, ModelUsage, Outcome, Resume, Run,
    RunRequest, SendError, ToolPolicy, Usage, WorkerSandbox,
};
use crate::routing;
use crate::server::Daemon;

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
        reply: oneshot::Sender<Result<AgentRun, ErrorObject>>,
    },
    /// `agent/cancel`.
    Cancel {
        reply: oneshot::Sender<Result<AgentRun, ErrorObject>>,
    },
    /// `agent/accept`.
    Accept {
        id: AcceptId,
        reviewed: Option<String>,
        reply: oneshot::Sender<Result<(AgentRun, AgentMerge), ErrorObject>>,
    },
}

struct Live {
    run: Arc<dyn Run>,
    events: EventStream,
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
    /// Messages sent to the run while this wispd runs, by turn id, for `agent/send`'s
    /// idempotency across CLI processes.
    turns: HashMap<TurnId, String>,
    /// The latest prompt or message, for the commit message.
    last_message: String,
    stopping: bool,
}

impl Actor {
    /// `turns` is what a run already sent, from the store (#190): empty for a run just created by
    /// `agents::start`, and loaded by `actor_for` for a run whose actor is spawned fresh, so a
    /// restarted wispd still recognizes a retried `agent/send`.
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
            last_message,
            stopping: false,
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

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>, shutdown: CancellationToken) {
        loop {
            let deadline = self.batch.since.map(|since| since + COALESCE);
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
                () = shutdown.cancelled(), if !self.stopping => {
                    self.stopping = true;
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
                event = next_event(&mut self.live) => self.on_event(event).await,
            }
            if self.stopping && self.live.is_none() {
                self.flush().await;
                break;
            }
        }
    }

    async fn on_command(&mut self, command: Command) {
        match command {
            Command::Send {
                turn_id,
                text,
                reply,
            } => {
                let answer = self.send(turn_id, text).await;
                let _ = reply.send(answer);
            }
            Command::Cancel { reply } => {
                if let Some(live) = &self.live {
                    info!(run = %self.id, "cancelling an agent run");
                    live.run.cancel();
                }
                let _ = reply.send(self.snapshot());
            }
            Command::Accept {
                id,
                reviewed,
                reply,
            } => {
                let answer = self.accept(id, reviewed).await;
                let _ = reply.send(answer);
            }
        }
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
        let refused = |why: String| ErrorObject::wisp(ErrorKind::MergeRefused, why);
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
        self.append(WispEvent::AgentAccepted {
            run_id: self.id,
            merge: merge.clone(),
        })
        .await;
        self.append(WispEvent::AgentUpdated {
            run_id: self.id,
            state: convert::run_state(&self.row),
        })
        .await;
        Ok((self.snapshot()?, merge))
    }

    async fn send(&mut self, turn_id: TurnId, text: String) -> Result<AgentRun, ErrorObject> {
        if text.trim().is_empty() {
            return Err(ErrorObject::invalid_params("text must not be empty"));
        }
        if self.accepted() {
            return Err(super::run_accepted(self.id));
        }
        if let Some(sent) = self.turns.get(&turn_id) {
            return if *sent == text {
                self.snapshot()
            } else {
                Err(ErrorObject::wisp(
                    ErrorKind::IdConflict,
                    format!("turn {turn_id} was already sent with a different text"),
                ))
            };
        }
        if let Some(live) = &self.live {
            let follow_up = FollowUp {
                turn_id,
                text: text.clone(),
            };
            match live.run.send(follow_up) {
                Ok(()) => {
                    self.record_turn(turn_id, text.clone()).await;
                    self.last_message = text;
                    return self.snapshot();
                }
                Err(SendError::IdConflict) => {
                    return Err(ErrorObject::wisp(
                        ErrorKind::IdConflict,
                        format!("turn {turn_id} was already sent with a different text"),
                    ));
                }
                Err(SendError::Unsupported) => {
                    return Err(ErrorObject::wisp(
                        ErrorKind::RunNotResumable,
                        "this run's backend takes no messages while it runs; send it again \
                         once the run has finished",
                    ));
                }
                // The CLI is exiting: let the run finish, then resume it with the message.
                Err(SendError::Finished) => {
                    while self.live.is_some() {
                        let event = next_event(&mut self.live).await;
                        self.on_event(event).await;
                    }
                }
            }
        }
        self.resume(turn_id, text).await
    }

    /// Starts a new CLI process for the run, resuming its vendor session with `text`.
    async fn resume(&mut self, turn_id: TurnId, text: String) -> Result<AgentRun, ErrorObject> {
        let Some(session_id) = self.row.state.session_id.clone() else {
            return Err(ErrorObject::wisp(
                ErrorKind::RunNotResumable,
                format!(
                    "run {} ended before its CLI reported a session, so it can't be resumed",
                    self.id
                ),
            ));
        };
        // The session belongs to the account the run was on when it ended, after any fallback,
        // not to whatever the worker role's default is now.
        let account = session_account(&self.row.state.account_id);
        let not_resumable = |why: String| {
            ErrorObject::wisp(
                ErrorKind::RunNotResumable,
                format!("run {} can't be resumed: {why}", self.id),
            )
        };
        let (prepared, _) = match prepare(&self.daemon, self.project, Some(account)).await {
            Ok(prepared) => prepared,
            Err(error)
                if error
                    .wisp_data()
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
        if backend != self.row.fields.backend {
            return Err(not_resumable(format!(
                "its session ran on {}, but its account now runs on {backend}",
                self.row.fields.backend
            )));
        }
        let session = session_id.clone();
        let totals = store(&self.daemon, move |db| {
            db.session_usage_totals(&session)
                .map_err(|error| store_error(&error))
        })
        .await?;
        let resume = Resume {
            session_id,
            usage_totals: totals.into_iter().map(model_usage).collect(),
        };
        info!(run = %self.id, "resuming an agent run's session");
        let message = text.clone();
        if self
            .launch(prepared, text, Some(turn_id), Some(resume), None)
            .await
        {
            // Only a turn that reached a CLI counts as sent: a retry after a failed start
            // tries again.
            self.record_turn(turn_id, message.clone()).await;
            self.last_message = message;
        }
        self.snapshot()
    }

    /// Records that `turn_id` was sent with `text`, in memory and in the store, so a retry of
    /// `agent/send` stays idempotent across a wispd restart, not only across a resumed CLI
    /// process within the same wispd (#190).
    /// Runs after the CLI has already accepted the turn (`live.run.send`'s `Ok`, or a successful
    /// `launch` in `resume`), so a crash between the two makes a retried `agent/send` after a
    /// restart send the message again: at-least-once, not exactly-once (#190 review non-blocking
    /// note). That's the same failure mode #190 was fixing in the other direction (a restart
    /// forgetting a turn was ever sent), and strictly better: a duplicate is visible in the
    /// transcript, a lost retry silently drops the user's message.
    async fn record_turn(&mut self, turn_id: TurnId, text: String) {
        self.turns.insert(turn_id, text.clone());
        let (run_id, id) = (self.row.id, self.id);
        let stored = store(&self.daemon, move |db| {
            db.record_turn(run_id, turn_id.into(), &text)
                .map_err(|error| store_error(&error))
        })
        .await;
        if let Err(error) = stored {
            warn!(run = %id, error = %error.message, "could not store a sent turn");
        }
    }

    /// Starts the run's CLI with `prompt` and records the result: `running`, or `failed` with
    /// why. `paths` are the worktree's and the repository git folder's canonical paths, when the
    /// caller already has them. Returns whether the CLI started.
    pub async fn launch(
        &mut self,
        prepared: Prepared,
        prompt: String,
        turn_id: Option<TurnId>,
        resume: Option<Resume>,
        paths: Option<(PathBuf, PathBuf)>,
    ) -> bool {
        let paths = match paths {
            Some(paths) => Ok(paths),
            None => self.worker_paths().await,
        };
        let (cwd, git_common_dir) = match paths {
            Ok(paths) => paths,
            Err(error) => {
                self.failed_to_start(error.message).await;
                return false;
            }
        };
        let Prepared {
            resolved,
            accounts,
            home,
            data_dir,
            context,
        } = prepared;
        let sandbox =
            WorkerSandbox::for_worktree(&home, &data_dir, &cwd, &git_common_dir, &context);
        let account_id = resolved.account_id();
        let request = RunRequest {
            run_id: self.id,
            turn_id,
            cwd,
            prompt,
            policy: ToolPolicy::WorkspaceWrite,
            sandbox: Some(sandbox),
            account: AccountRef {
                id: account_id.clone(),
                credential: Credential::Subscription { config_home: None },
            },
            resume,
            model: None,
        };
        match routing::start(Arc::clone(&self.daemon.keys), &accounts, resolved, request) {
            Ok(started) => {
                self.live = Some(Live {
                    run: started.run,
                    events: started.events,
                });
                self.daemon.agents.running.fetch_add(1, Ordering::Relaxed);
                convert::RUNNING.clone_into(&mut self.row.state.status);
                self.row.state.account_id = account_id;
                self.row.state.error = None;
                self.save().await;
                true
            }
            Err(error) => {
                self.failed_to_start(error.to_string()).await;
                false
            }
        }
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
            .map_err(|error| ErrorObject::wisp(ErrorKind::WorktreeFailed, error.to_string()))?;
        let git_dir = sandbox_path(&git_dir, "the repository's git folder")?;
        Ok((cwd, git_dir))
    }

    async fn failed_to_start(&mut self, message: String) {
        warn!(run = %self.id, %message, "an agent run's CLI could not start");
        self.append(WispEvent::AgentFinished {
            run_id: self.id,
            outcome: AgentOutcome::Failed {
                failure: AgentFailureKind::SpawnFailed,
                message: message.clone(),
            },
        })
        .await;
        convert::FAILED.clone_into(&mut self.row.state.status);
        self.row.state.error = Some(message);
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
                self.append(WispEvent::AgentAccountFallback {
                    run_id: self.id,
                    from_account: from_account.clone(),
                    to_account: to_account.clone(),
                    reason: convert::failure_kind(*reason),
                })
                .await;
                self.row.state.account_id.clone_from(to_account);
                self.save().await;
            }
            Event::Usage(_) | Event::RateLimit(_) => {
                self.record_usage(event.clone()).await;
                if let Some(item) = output_item(&event) {
                    self.push(item).await;
                }
            }
            Event::Finished { outcome, .. } => {
                let outcome = outcome.clone();
                self.record_usage(event).await;
                self.clear_live();
                self.finish(&outcome).await;
            }
            _ => {
                if let Some(item) = output_item(&event) {
                    self.push(item).await;
                }
            }
        }
    }

    fn clear_live(&mut self) {
        if self.live.take().is_some() {
            self.daemon.agents.running.fetch_sub(1, Ordering::Relaxed);
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

    /// Records how a CLI process ended. Unless wispd stopped it, commits the worktree's changes
    /// first, through #166's hardened commit, and reports the commit.
    async fn finish(&mut self, outcome: &Outcome) {
        self.flush().await;
        if self.stopping && matches!(outcome, Outcome::Cancelled) {
            info!(run = %self.id, "an agent run was interrupted because wispd is stopping");
            self.append(WispEvent::AgentFinished {
                run_id: self.id,
                outcome: AgentOutcome::Interrupted,
            })
            .await;
            convert::INTERRUPTED.clone_into(&mut self.row.state.status);
            self.save().await;
            return;
        }
        let (mut outcome, mut status, mut error) = convert::outcome(outcome);
        let committed = self.commit().await;
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
        self.append(WispEvent::AgentFinished {
            run_id: self.id,
            outcome,
        })
        .await;
        if let Some(diff) = diff {
            self.row.state.commit_sha = Some(diff.commit.clone());
            self.row.state.files_changed = Some(diff.files);
            self.row.state.insertions = Some(diff.insertions);
            self.row.state.deletions = Some(diff.deletions);
            self.append(WispEvent::AgentDiffReady {
                run_id: self.id,
                diff,
            })
            .await;
        }
        status.clone_into(&mut self.row.state.status);
        self.row.state.error = error;
        info!(run = %self.id, status, "an agent run's CLI finished");
        self.save().await;
    }

    /// Commits whatever the run changed in its worktree, on its branch, and measures the branch
    /// against the worktree's base. `None` when there was nothing new to commit.
    async fn commit(&self) -> Result<Option<DiffSummary>, String> {
        let Some(worktree) = &self.worktree else {
            return Err("the run was accepted, and its worktree is gone".to_owned());
        };
        if worktree.git_dir.is_empty() {
            return Err(
                "the run's worktree has no recorded git folder, so wispd can't commit it safely"
                    .to_owned(),
            );
        }
        let worktrees = &self.daemon.agents.worktrees;
        let path = Path::new(&worktree.path);
        let git_dir = Path::new(&worktree.git_dir);
        let message = commit_message(&self.last_message, self.id);
        let commit = worktrees
            .commit_all(path, git_dir, Path::new(&worktree.repo_path), &message)
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
            self.append(WispEvent::AgentOutput {
                run_id: self.id,
                items: batch.items,
            })
            .await;
        }
    }

    /// From a tokio task: the event log's own writer thread does the SQLite work, so awaiting it
    /// here yields this actor's worker thread to other work instead of blocking it (#190).
    async fn append(&self, event: WispEvent) -> u64 {
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
        self.append(WispEvent::AgentUpdated {
            run_id: self.id,
            state: convert::run_state(&self.row),
        })
        .await;
    }
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

/// The merge commit's message, when accepting a run needs one: `Merge wisp run: <the task's first
/// line>`, cut to 72 characters, then the run and its branch.
fn merge_message(prompt: &str, run: RunId, branch: &str) -> String {
    let first = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("agent run");
    let mut subject: String = format!("Merge wisp run: {}", first.trim());
    if subject.chars().count() > 72 {
        subject = subject.chars().take(69).collect::<String>() + "...";
    }
    format!("{subject}\n\nAccepted in wisp: agent run {run}, branch {branch}.\n")
}

fn accept_error(error: &crate::worktree::AcceptError) -> ErrorObject {
    use crate::worktree::AcceptError;
    match error {
        AcceptError::Refused(message) => ErrorObject::wisp(ErrorKind::MergeRefused, message),
        AcceptError::Conflict { .. } => {
            ErrorObject::wisp(ErrorKind::MergeConflict, error.to_string())
        }
        AcceptError::Git(error) => ErrorObject::wisp(ErrorKind::MergeRefused, error.to_string()),
    }
}

/// `wisp: <the message's first line>`, cut to 72 characters, then the run it belongs to.
fn commit_message(message: &str, run: RunId) -> String {
    let first = message
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("agent run");
    let mut subject: String = format!("wisp: {}", first.trim());
    if subject.chars().count() > 72 {
        subject = subject.chars().take(69).collect::<String>() + "...";
    }
    format!("{subject}\n\nCommitted by wisp for agent run {run}.\n")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::{mpsc, oneshot};
    use tokio_util::sync::CancellationToken;
    use wisp_protocol::{AccountChoice, AccountId, ProjectId, RunId};
    use wisp_store::{Run as RunRow, RunFields, RunState, Worktree};

    use super::{Actor, Command, Live, commit_message, session_account};
    use crate::backend::{Event, EventSink, FollowUp, Run, SendError};
    use crate::server::Daemon;

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
        assert!(message.starts_with("wisp: Add a README\n\n"), "{message}");
        assert!(message.contains(&run.to_string()));
        let long = commit_message(&"x".repeat(200), run);
        assert_eq!(long.lines().next().unwrap().chars().count(), 72);
        assert!(commit_message("", run).starts_with("wisp: agent run"));
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
            branch: "wisp/run".to_owned(),
            base: "0".repeat(40),
            git_dir: String::new(),
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
}
