//! The M3 runner (#156, decision 0014): runs a worker end to end.
//!
//! `agent/start` resolves the worker's account through routing (#119), refuses a worker wispd
//! can't sandbox (0013), creates the run's worktree (#154), records the run, and starts the
//! backend in the worktree with the project's shared context folder (#155) writable. From then
//! on one [`actor`] task per run owns it: it streams the backend's events into the event log as
//! `agent.*` events, records usage (#120) against whichever account the run is on, takes
//! `agent/send` and `agent/cancel`, and when a CLI process ends, commits the worktree through
//! #166's hardened `commit_all` and reports `agent.diffReady`.
//!
//! A run outlives its CLI processes: `agent/send` to a run whose CLI has ended resumes the
//! vendor session in the same worktree. When wispd stops, running CLIs are cancelled and their
//! runs recorded `interrupted`; a run still `starting` or `running` in the store when wispd
//! starts (a crash) is marked `interrupted` too. Either kind resumes through `agent/send`.

mod actor;
mod convert;
pub(crate) mod worker;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{error, info, warn};
use uuid::Uuid;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AccountChoice, AgentOutcome, AgentRun, AgentSendParams, AgentStartParams, ErrorKind, ProjectId,
    Role, RunId, WispEvent,
};
use wisp_store::{RunFields, RunState, StoreError, WorktreeFields};

use self::actor::{Actor, Command};
pub(crate) use self::convert::agent_run as snapshot;
use self::convert::{STARTING, WORKSPACE_WRITE, agent_run};
use self::worker::{StoredKeyAccounts, sandbox_path, worker_unavailable};
use crate::backend::ToolPolicy;
use crate::routing::{self, BackendRegistry, Defaults, Resolved, RoutingError};
use crate::server::Daemon;
use crate::worktree::{CreatedWorktree, WorktreeError, WorktreeManager};

/// How long a stopping wispd waits for its runs to record that they were interrupted.
const SHUTDOWN_WAIT: Duration = Duration::from_secs(15);

/// Every run's actor, and what they share.
pub(crate) struct Agents {
    backends: BackendRegistry,
    worktrees: WorktreeManager,
    actors: Mutex<HashMap<RunId, mpsc::Sender<Command>>>,
    /// Held while a run is created, and while an actor is spawned for a run created earlier, so
    /// one run never gets two worktrees or two actors.
    start_lock: tokio::sync::Mutex<()>,
    running: AtomicU32,
    tracker: TaskTracker,
    shutdown: CancellationToken,
}

impl std::fmt::Debug for Agents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agents")
            .field("backends", &self.backends)
            .field("running", &self.running())
            .finish_non_exhaustive()
    }
}

/// What starting a worker's CLI needs, from [`prepare`].
pub(super) struct Prepared {
    resolved: Resolved,
    accounts: StoredKeyAccounts,
    home: PathBuf,
    data_dir: PathBuf,
    context: PathBuf,
}

impl Agents {
    /// A runner that starts workers on `backends`, in worktrees `worktrees` makes.
    pub fn new(backends: BackendRegistry, worktrees: WorktreeManager) -> Self {
        Self {
            backends,
            worktrees,
            actors: Mutex::new(HashMap::new()),
            start_lock: tokio::sync::Mutex::new(()),
            running: AtomicU32::new(0),
            tracker: TaskTracker::new(),
            shutdown: CancellationToken::new(),
        }
    }

    /// How many runs have a CLI running, for `host/health`.
    pub fn running(&self) -> u32 {
        self.running.load(Ordering::Relaxed)
    }

    /// Runs `task` to the end even if the request that started it is dropped, as when its
    /// connection closes: a disconnect never stops an agent (0007).
    pub async fn detached<T: Send + 'static>(
        &self,
        task: impl Future<Output = Result<T, ErrorObject>> + Send + 'static,
    ) -> Result<T, ErrorObject> {
        self.tracker.spawn(task).await.map_err(|error| {
            ErrorObject::internal_error(format!("the run's task failed: {error}"))
        })?
    }

    fn actor(&self, id: RunId) -> Option<mpsc::Sender<Command>> {
        self.actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&id)
            .cloned()
    }

    fn spawn(&self, actor: Actor) -> mpsc::Sender<Command> {
        let (commands, receiver) = mpsc::channel(16);
        self.actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(actor.id(), commands.clone());
        self.tracker
            .spawn(actor.run(receiver, self.shutdown.clone()));
        commands
    }

    /// Stops every running CLI, and waits a while for their runs to record that they were
    /// interrupted. Called once, when wispd stops, before the store closes.
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.tracker.close();
        if tokio::time::timeout(SHUTDOWN_WAIT, self.tracker.wait())
            .await
            .is_err()
        {
            warn!("some agent runs did not record that they were interrupted in time");
        }
    }
}

/// Stores `job`'s answer from the store's thread. The runner's store jobs are never cancelled:
/// a run's bookkeeping has to happen whether or not anyone is still waiting for it.
async fn store<T: Send + 'static>(
    daemon: &Daemon,
    job: impl FnOnce(&mut wisp_store::Store) -> Result<T, ErrorObject> + Send + 'static,
) -> Result<T, ErrorObject> {
    daemon.store.run(&CancellationToken::new(), job).await
}

pub(crate) fn store_error(error: &StoreError) -> ErrorObject {
    error!(%error, "the project store failed for an agent run");
    ErrorObject::internal_error(format!("the project store failed: {error}"))
}

fn run_not_found(id: RunId) -> ErrorObject {
    ErrorObject::wisp(ErrorKind::RunNotFound, format!("no agent run has id {id}"))
}

fn requested_account(account: Option<&AccountChoice>) -> Option<String> {
    account.and_then(|account| serde_json::to_string(account).ok())
}

/// The project's repository, the routing inputs, and the paths a worker for `project` needs,
/// checked: everything that can refuse a worker before anything is created.
pub(super) async fn prepare(
    daemon: &Arc<Daemon>,
    project: ProjectId,
    requested: Option<AccountChoice>,
) -> Result<(Prepared, String), ErrorObject> {
    let (repo_path, defaults, accounts) = store(daemon, move |db| {
        let row = db
            .get_project(project.into())
            .map_err(|error| store_error(&error))?
            .ok_or_else(|| {
                ErrorObject::wisp(
                    ErrorKind::ProjectNotFound,
                    format!("no project has id {project}"),
                )
            })?;
        let defaults = crate::methods::read_defaults(db)?;
        let mut accounts = HashMap::new();
        for account in db.list_accounts().map_err(|error| store_error(&error))? {
            let account = crate::store::key_account(account)?;
            accounts.insert(account.id, account.provider);
        }
        Ok((row.repo_path, defaults, StoredKeyAccounts(accounts)))
    })
    .await?;
    let defaults = Defaults {
        coordinator: defaults.coordinator,
        worker: defaults.worker,
    };
    let resolved = routing::resolve(
        &daemon.agents.backends,
        &accounts,
        &defaults,
        Role::Worker,
        requested,
        ToolPolicy::WorkspaceWrite,
    )
    .map_err(|error| routing_error(&error))?;
    worker::check_backend(resolved.backend())?;
    if let Some(cli) = worker::cli_of(resolved.backend()) {
        let find = |clis: &[wisp_protocol::DetectedCli]| {
            clis.iter().find(|detected| detected.cli == cli).cloned()
        };
        let cached = find(&daemon.cli_detector.list().await.clis);
        if worker::check_claude(cached.as_ref()).is_err() {
            // The user may have just updated the CLI: look again before refusing.
            let fresh = find(&daemon.cli_detector.refresh().await.clis);
            worker::check_claude(fresh.as_ref())?;
        }
    }
    let home = worker::home()?;
    let data_dir = sandbox_path(daemon.data_dir.root(), "wispd's data folder")?;
    let context = crate::context::ensure_dir(&daemon.data_dir, project).map_err(|error| {
        worker_unavailable(format!(
            "could not create the shared context folder: {error}"
        ))
    })?;
    let context = sandbox_path(&context, "the shared context folder")?;
    sandbox_path(Path::new(&repo_path), "the project's repository")?;
    let prepared = Prepared {
        resolved,
        accounts,
        home,
        data_dir,
        context,
    };
    Ok((prepared, repo_path))
}

fn routing_error(error: &RoutingError) -> ErrorObject {
    match error {
        RoutingError::NoAccount { .. } => ErrorObject::invalid_params(
            "no account was named, and the worker role has no default; set one with \
             accounts/defaults/set",
        ),
        &RoutingError::UnknownKeyAccount { id } => ErrorObject::wisp(
            ErrorKind::AccountNotFound,
            format!("no key account has id {id}"),
        ),
        RoutingError::UnknownBackend { .. } | RoutingError::UnknownProvider { .. } => {
            worker_unavailable(error.to_string())
        }
        RoutingError::UnknownChoice => {
            ErrorObject::invalid_params("account must be a subscription or a key")
        }
    }
}

fn worktree_failed(error: &WorktreeError) -> ErrorObject {
    ErrorObject::wisp(ErrorKind::WorktreeFailed, error.to_string())
}

/// The run `run_id` already is, for a retry of `agent/start` with the same params, or
/// `idConflict` if they differ. `None` for a new run.
async fn existing(
    daemon: &Arc<Daemon>,
    run_id: RunId,
    project: ProjectId,
    prompt: &str,
    requested: Option<&str>,
) -> Result<Option<AgentRun>, ErrorObject> {
    let found = store(daemon, move |db| {
        let Some(row) = db.get_run(run_id.into()).map_err(|e| store_error(&e))? else {
            return Ok(None);
        };
        let worktree = db
            .get_worktree(run_id.into())
            .map_err(|e| store_error(&e))?;
        Ok(Some((row, worktree)))
    })
    .await?;
    let Some((row, worktree)) = found else {
        return Ok(None);
    };
    let same = row.fields.project_id == Uuid::from(project)
        && row.fields.prompt == prompt
        && row.fields.policy == WORKSPACE_WRITE
        && row.fields.requested_account.as_deref() == requested;
    if !same {
        return Err(ErrorObject::wisp(
            ErrorKind::IdConflict,
            format!("run {run_id} exists with a different project, prompt, account, or policy"),
        ));
    }
    agent_run(&row, worktree.as_ref()).map(Some)
}

/// Creates `run_id`'s worktree of `repo_path`, and returns it with its canonical path and the
/// repository's shared git folder, both checked for the sandbox. A worktree whose paths the
/// sandbox can't hold is removed again.
async fn create_worktree(
    agents: &Agents,
    repo_path: &Path,
    run_id: RunId,
) -> Result<(CreatedWorktree, PathBuf, PathBuf), ErrorObject> {
    let created = agents
        .worktrees
        .create(repo_path, run_id, None)
        .await
        .map_err(|error| worktree_failed(&error))?;
    let paths = async {
        let worktree = sandbox_path(&created.path, "the run's worktree")?;
        let common = agents
            .worktrees
            .git_common_dir(repo_path)
            .await
            .map_err(|error| worktree_failed(&error))?;
        let common = sandbox_path(&common, "the repository's git folder")?;
        Ok::<_, ErrorObject>((worktree, common))
    }
    .await;
    match paths {
        Ok((worktree, common)) => Ok((created, worktree, common)),
        Err(error) => {
            if let Err(cleanup) = agents
                .worktrees
                .remove(repo_path, &created.path, &created.branch)
                .await
            {
                warn!(run = %run_id, %cleanup, "could not remove a worktree for a run that didn't start");
            }
            Err(error)
        }
    }
}

/// `agent/start`: see the module documentation. Idempotent on the run id.
pub(crate) async fn start(
    daemon: Arc<Daemon>,
    params: AgentStartParams,
) -> Result<AgentRun, ErrorObject> {
    let agents = &daemon.agents;
    let _creating = agents.start_lock.lock().await;
    let AgentStartParams {
        run_id,
        project,
        prompt,
        account,
        ..
    } = params;
    let requested = requested_account(account.as_ref());

    if let Some(run) = existing(&daemon, run_id, project, &prompt, requested.as_deref()).await? {
        return Ok(run);
    }
    let (prepared, repo_path) = prepare(&daemon, project, account).await?;
    let (created, worktree_path, git_common_dir) =
        create_worktree(agents, Path::new(&repo_path), run_id).await?;

    let fields = RunFields {
        project_id: project.into(),
        prompt: prompt.clone(),
        requested_account: requested,
        policy: WORKSPACE_WRITE.to_owned(),
        backend: prepared.resolved.backend().name().to_owned(),
    };
    let state = RunState {
        status: STARTING.to_owned(),
        account_id: prepared.resolved.account_id(),
        ..RunState::default()
    };
    let worktree_fields = WorktreeFields {
        repo_path: repo_path.clone(),
        path: created.path.to_string_lossy().into_owned(),
        branch: created.branch.clone(),
        base: created.base.clone(),
        git_dir: created.git_dir.to_string_lossy().into_owned(),
    };
    let recorded = store(&daemon, move |db| {
        let worktree = db
            .create_worktree(run_id.into(), &worktree_fields)
            .map_err(|e| store_error(&e))?;
        let row = db
            .create_run(run_id.into(), &fields, &state)
            .map_err(|e| store_error(&e))?;
        Ok((row, worktree))
    })
    .await;
    let (row, worktree) = match recorded {
        Ok(recorded) => recorded,
        Err(error) => {
            if let Err(cleanup) = agents
                .worktrees
                .remove(Path::new(&repo_path), &created.path, &created.branch)
                .await
            {
                warn!(run = %run_id, %cleanup, "could not remove a worktree for a run that wasn't recorded");
            }
            return Err(error);
        }
    };
    let snapshot = agent_run(&row, Some(&worktree))?;
    daemon.log.append(
        snapshot.created_at,
        Some(project),
        WispEvent::AgentStarted {
            run_id,
            run: Some(snapshot),
        },
    );
    info!(run = %run_id, project = %project, backend = %row.fields.backend, "created an agent run");

    let mut actor = Actor::new(Arc::clone(&daemon), row, worktree);
    let task = worker::worker_prompt(&prompt, &worktree_path, &prepared.context);
    actor
        .launch(
            prepared,
            task,
            None,
            None,
            Some((worktree_path, git_common_dir)),
        )
        .await;
    let run = actor.snapshot()?;
    agents.spawn(actor);
    Ok(run)
}

/// The command channel of `id`'s actor, spawning one for a run created before this wispd
/// started.
async fn actor_for(daemon: &Arc<Daemon>, id: RunId) -> Result<mpsc::Sender<Command>, ErrorObject> {
    let agents = &daemon.agents;
    if let Some(actor) = agents.actor(id) {
        return Ok(actor);
    }
    let _creating = agents.start_lock.lock().await;
    if let Some(actor) = agents.actor(id) {
        return Ok(actor);
    }
    let (row, worktree) = store(daemon, move |db| {
        let row = db
            .get_run(id.into())
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| run_not_found(id))?;
        let worktree = db
            .get_worktree(id.into())
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| {
                ErrorObject::internal_error(format!("run {id} has no recorded worktree"))
            })?;
        Ok((row, worktree))
    })
    .await?;
    Ok(agents.spawn(Actor::new(Arc::clone(daemon), row, worktree)))
}

async fn ask(
    daemon: &Arc<Daemon>,
    id: RunId,
    command: impl FnOnce(oneshot::Sender<Result<AgentRun, ErrorObject>>) -> Command,
) -> Result<AgentRun, ErrorObject> {
    let actor = actor_for(daemon, id).await?;
    let (reply, answer) = oneshot::channel();
    let stopping = || ErrorObject::internal_error("wispd is stopping");
    actor.send(command(reply)).await.map_err(|_| stopping())?;
    answer.await.map_err(|_| stopping())?
}

/// `agent/send`.
pub(crate) async fn send(
    daemon: Arc<Daemon>,
    params: AgentSendParams,
) -> Result<AgentRun, ErrorObject> {
    let AgentSendParams {
        run_id,
        turn_id,
        text,
    } = params;
    ask(&daemon, run_id, |reply| Command::Send {
        turn_id,
        text,
        reply,
    })
    .await
}

/// `agent/cancel`.
pub(crate) async fn cancel(daemon: Arc<Daemon>, id: RunId) -> Result<AgentRun, ErrorObject> {
    ask(&daemon, id, |reply| Command::Cancel { reply }).await
}

/// Marks every run the store still has as `starting` or `running` as `interrupted`: wispd
/// stopped without recording how they ended, as after a crash. Called once at startup, before
/// any connection is accepted.
pub(crate) async fn recover(daemon: &Arc<Daemon>) {
    let recovered = store(daemon, |db| {
        let mut recovered = Vec::new();
        for row in db.list_runs(None).map_err(|e| store_error(&e))? {
            if row.state.status != convert::STARTING && row.state.status != convert::RUNNING {
                continue;
            }
            let state = RunState {
                status: convert::INTERRUPTED.to_owned(),
                ..row.state.clone()
            };
            let row = db.update_run(row.id, &state).map_err(|e| store_error(&e))?;
            let worktree = db.get_worktree(row.id).map_err(|e| store_error(&e))?;
            recovered.push(agent_run(&row, worktree.as_ref())?);
        }
        Ok(recovered)
    })
    .await;
    match recovered {
        Ok(runs) => {
            for run in runs {
                info!(run = %run.id, "an agent run was interrupted when wispd last stopped");
                daemon.log.append(
                    run.updated_at,
                    Some(run.project),
                    WispEvent::AgentFinished {
                        run_id: run.id,
                        outcome: AgentOutcome::Interrupted,
                    },
                );
                daemon.log.append(
                    run.updated_at,
                    Some(run.project),
                    WispEvent::AgentUpdated {
                        run_id: run.id,
                        run,
                    },
                );
            }
        }
        Err(error) => warn!(error = %error.message, "could not recover interrupted agent runs"),
    }
}
