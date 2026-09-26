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
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AccountChoice, AgentFailureKind, AgentOutcome, AgentOutputItem, AgentRun, DiffSummary,
    ErrorKind, ProjectId, RunId, TurnId, WispEvent,
};
use wisp_store::{Run as RunRow, SessionModelUsage, Worktree};

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
    worktree: Worktree,
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
    pub fn new(daemon: Arc<Daemon>, row: RunRow, worktree: Worktree) -> Self {
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
            turns: HashMap::new(),
            last_message,
            stopping: false,
        }
    }

    pub fn id(&self) -> RunId {
        self.id
    }

    pub fn snapshot(&self) -> Result<AgentRun, ErrorObject> {
        agent_run(&self.row, Some(&self.worktree))
    }

    pub async fn run(mut self, mut commands: mpsc::Receiver<Command>, shutdown: CancellationToken) {
        loop {
            let deadline = self.batch.since.map(|since| since + COALESCE);
            tokio::select! {
                biased;
                () = shutdown.cancelled(), if !self.stopping => {
                    self.stopping = true;
                    if let Some(live) = &self.live {
                        live.run.cancel();
                    }
                }
                event = next_event(&mut self.live) => self.on_event(event).await,
                command = commands.recv(), if !self.stopping => match command {
                    Some(command) => self.on_command(command).await,
                    None => self.stopping = true,
                },
                () = sleep_until(deadline.unwrap_or_else(Instant::now)), if deadline.is_some() => {
                    self.flush();
                }
            }
            if self.stopping && self.live.is_none() {
                self.flush();
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
        }
    }

    async fn send(&mut self, turn_id: TurnId, text: String) -> Result<AgentRun, ErrorObject> {
        if text.trim().is_empty() {
            return Err(ErrorObject::invalid_params("text must not be empty"));
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
                    self.turns.insert(turn_id, text.clone());
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
        let requested = match &self.row.fields.requested_account {
            Some(json) => Some(
                serde_json::from_str::<AccountChoice>(json).map_err(|error| {
                    ErrorObject::internal_error(format!(
                        "the run's stored account is invalid: {error}"
                    ))
                })?,
            ),
            None => None,
        };
        let (prepared, _) = prepare(&self.daemon, self.project, requested).await?;
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
        self.turns.insert(turn_id, text.clone());
        self.last_message.clone_from(&text);
        info!(run = %self.id, "resuming an agent run's session");
        self.launch(prepared, text, Some(turn_id), Some(resume), None)
            .await;
        self.snapshot()
    }

    /// Starts the run's CLI with `prompt` and records the result: `running`, or `failed` with
    /// why. `paths` are the worktree's and the repository git folder's canonical paths, when the
    /// caller already has them.
    pub async fn launch(
        &mut self,
        prepared: Prepared,
        prompt: String,
        turn_id: Option<TurnId>,
        resume: Option<Resume>,
        paths: Option<(PathBuf, PathBuf)>,
    ) {
        let paths = match paths {
            Some(paths) => Ok(paths),
            None => self.worker_paths().await,
        };
        let (cwd, git_common_dir) = match paths {
            Ok(paths) => paths,
            Err(error) => {
                self.failed_to_start(error.message).await;
                return;
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
            }
            Err(error) => self.failed_to_start(error.to_string()).await,
        }
    }

    async fn worker_paths(&self) -> Result<(PathBuf, PathBuf), ErrorObject> {
        let cwd = sandbox_path(Path::new(&self.worktree.path), "the run's worktree")?;
        let git_dir = self
            .daemon
            .agents
            .worktrees
            .git_common_dir(Path::new(&self.worktree.repo_path))
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
        });
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
                    self.push(item);
                }
                self.row.state.session_id = Some(session_id.clone());
                self.save().await;
            }
            Event::AccountFallback {
                from_account,
                to_account,
                reason,
            } => {
                self.flush();
                self.append(WispEvent::AgentAccountFallback {
                    run_id: self.id,
                    from_account: from_account.clone(),
                    to_account: to_account.clone(),
                    reason: convert::failure_kind(*reason),
                });
                self.row.state.account_id.clone_from(to_account);
                self.save().await;
            }
            Event::Usage(_) | Event::RateLimit(_) => {
                self.record_usage(event.clone()).await;
                if let Some(item) = output_item(&event) {
                    self.push(item);
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
                    self.push(item);
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

    /// Records how a CLI process ended. Unless wispd is stopping, commits the worktree's changes
    /// first, through #166's hardened commit, and reports the commit.
    async fn finish(&mut self, outcome: &Outcome) {
        self.flush();
        if self.stopping {
            info!(run = %self.id, "an agent run was interrupted because wispd is stopping");
            self.append(WispEvent::AgentFinished {
                run_id: self.id,
                outcome: AgentOutcome::Interrupted,
            });
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
        });
        if let Some(diff) = diff {
            self.row.state.commit_sha = Some(diff.commit.clone());
            self.row.state.files_changed = Some(diff.files);
            self.row.state.insertions = Some(diff.insertions);
            self.row.state.deletions = Some(diff.deletions);
            self.append(WispEvent::AgentDiffReady {
                run_id: self.id,
                diff,
            });
        }
        status.clone_into(&mut self.row.state.status);
        self.row.state.error = error;
        info!(run = %self.id, status, "an agent run's CLI finished");
        self.save().await;
    }

    /// Commits whatever the run changed in its worktree, on its branch, and measures the branch
    /// against the worktree's base. `None` when there was nothing new to commit.
    async fn commit(&self) -> Result<Option<DiffSummary>, String> {
        if self.worktree.git_dir.is_empty() {
            return Err(
                "the run's worktree has no recorded git folder, so wispd can't commit it safely"
                    .to_owned(),
            );
        }
        let worktrees = &self.daemon.agents.worktrees;
        let path = Path::new(&self.worktree.path);
        let git_dir = Path::new(&self.worktree.git_dir);
        let message = commit_message(&self.last_message, self.id);
        let commit = worktrees
            .commit_all(path, git_dir, Path::new(&self.worktree.repo_path), &message)
            .await
            .map_err(|error| error.to_string())?;
        let Some(commit) = commit else {
            return Ok(None);
        };
        let stat = worktrees
            .diff_stat(path, git_dir, &self.worktree.base)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Some(DiffSummary {
            commit: commit.sha,
            files: stat.files,
            insertions: stat.insertions,
            deletions: stat.deletions,
        }))
    }

    fn push(&mut self, item: AgentOutputItem) {
        self.batch.bytes += item_bytes(&item);
        self.batch.items.push(item);
        self.batch.since.get_or_insert_with(Instant::now);
        if self.batch.bytes >= MAX_BATCH_BYTES {
            self.flush();
        }
    }

    fn flush(&mut self) {
        let batch = std::mem::take(&mut self.batch);
        if !batch.items.is_empty() {
            self.append(WispEvent::AgentOutput {
                run_id: self.id,
                items: batch.items,
            });
        }
    }

    fn append(&self, event: WispEvent) -> u64 {
        self.daemon
            .log
            .append(jiff::Timestamp::now(), Some(self.project), event)
    }

    /// Stores the run's state and reports it as `agent.updated`, after any transcript items
    /// waiting to be sent.
    async fn save(&mut self) {
        self.flush();
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
        match self.snapshot() {
            Ok(run) => {
                self.append(WispEvent::AgentUpdated {
                    run_id: self.id,
                    run,
                });
            }
            Err(error) => {
                warn!(run = %self.id, error = %error.message, "could not report an agent run");
            }
        }
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
    use wisp_protocol::RunId;

    use super::commit_message;

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
}
