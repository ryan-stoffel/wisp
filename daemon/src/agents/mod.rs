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
pub(crate) mod review;
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
    AccountChoice, AgentAcceptParams, AgentAcceptResult, AgentOutcome, AgentRun, AgentRunState,
    AgentSendParams, AgentStartParams, ErrorKind, ProjectId, Role, RunId, TurnId, WispEvent,
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
    /// One lock per run id, held while that run is being created, or while its actor is spawned
    /// for a run created earlier, so one run never gets two worktrees or two actors (#190: a run
    /// id's lock never makes an unrelated run's `agent/start`, `agent/send`, or `agent/cancel`
    /// wait, unlike the single lock this replaced).
    starting: StartLocks,
    running: AtomicU32,
    tracker: TaskTracker,
    shutdown: CancellationToken,
}

/// Per-run-id locks for [`Agents::starting`] (#190).
#[derive(Default)]
struct StartLocks {
    locks: Mutex<HashMap<RunId, Arc<tokio::sync::Mutex<()>>>>,
}

impl StartLocks {
    /// `run_id`'s lock, creating one if this is the first caller to ask for it.
    ///
    /// Also sweeps every entry nothing holds any more (#190 review): a waiter whose own task was
    /// cancelled while queued on `.lock_owned().await` never runs `release`, since it never got
    /// as far as constructing a `Starting` to drop — its `Arc` simply disappears when its future
    /// does, which `release` alone can't observe. Left alone, such an entry would sit in the map
    /// forever holding a lock nobody can ever take again. This sweep, run on every `get`, catches
    /// it: nothing but the map's own clone remains, so `strong_count` is 1.
    fn get(&self, run_id: RunId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        locks.retain(|_, lock| Arc::strong_count(lock) > 1);
        Arc::clone(
            locks
                .entry(run_id)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    /// Drops `run_id`'s entry, but only if nothing besides this map and the caller's own
    /// about-to-be-dropped guard still holds it (#190 review): removing it unconditionally would
    /// let a fresh caller's `get` hand out a *different*, uncontended lock while another caller
    /// that queued earlier is still waiting on the old one, so both could end up inside the
    /// critical section together — exactly the double-worktree, double-actor race this whole
    /// mechanism exists to prevent, and one that only shows up when the first attempt fails
    /// (`existing()`'s fast path in `start`, and `agents.actor(id)` in `actor_for`, both have
    /// nothing to find until a start actually succeeds). `Starting::drop` calls this while its own
    /// `OwnedMutexGuard` is still alive, so a caller with no other waiters sees `strong_count == 2`
    /// (the map's clone and that guard's); anything higher means a waiter is still queued, and the
    /// entry is left for `get`'s sweep to clean up once every waiter is done with it.
    fn release(&self, run_id: RunId) {
        let mut locks = self.locks.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(lock) = locks.get(&run_id)
            && Arc::strong_count(lock) <= 2
        {
            locks.remove(&run_id);
        }
    }
}

/// Held for as long as `run_id` is being created or its actor spawned; releases the per-run lock
/// on drop, from whichever exit path (#190).
pub(super) struct Starting<'a> {
    agents: &'a Agents,
    run_id: RunId,
    _lock: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for Starting<'_> {
    fn drop(&mut self) {
        self.agents.starting.release(self.run_id);
    }
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
            starting: StartLocks::default(),
            running: AtomicU32::new(0),
            tracker: TaskTracker::new(),
            shutdown: CancellationToken::new(),
        }
    }

    /// Locks `run_id`'s per-run start lock, waiting only on another call for the same run id
    /// (#190). Also used by `Actor::delete` (#110) to hold off a concurrent `create`/`actor_for`
    /// retry for this exact run id while its rows are deleted and its actor dropped: since a
    /// live actor's own fast path (`agents.actor(id)`) never reaches this lock, nothing here
    /// waits on an unrelated run's.
    pub(super) async fn start_guard(&self, run_id: RunId) -> Starting<'_> {
        let lock = self.starting.get(run_id).lock_owned().await;
        Starting {
            agents: self,
            run_id,
            _lock: lock,
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

pub(super) fn run_not_found(id: RunId) -> ErrorObject {
    ErrorObject::wisp(ErrorKind::RunNotFound, format!("no agent run has id {id}"))
}

pub(crate) fn run_accepted(id: RunId) -> ErrorObject {
    ErrorObject::wisp(
        ErrorKind::RunAccepted,
        format!("run {id} was accepted; its worktree and branch are gone"),
    )
}

fn requested_account(account: Option<&AccountChoice>) -> Option<String> {
    account.and_then(|account| serde_json::to_string(account).ok())
}

/// The project's repository, the routing inputs, and the paths run `run` of `project` needs,
/// checked: everything that can refuse a worker before anything is created.
pub(super) async fn prepare(
    daemon: &Arc<Daemon>,
    project: ProjectId,
    run: RunId,
    requested: Option<AccountChoice>,
) -> Result<(Prepared, String), ErrorObject> {
    let (repo_path, context_scope, defaults, accounts) = store(daemon, move |db| {
        let repo_path = crate::threads::scope_path(db, project)?;
        let context_scope = crate::threads::context_scope(db, project, run)?;
        let defaults = crate::methods::read_defaults(db)?;
        let mut accounts = HashMap::new();
        for account in db.list_accounts().map_err(|error| store_error(&error))? {
            let account = crate::store::key_account(account)?;
            accounts.insert(account.id, account.provider);
        }
        Ok((
            repo_path,
            context_scope,
            defaults,
            StoredKeyAccounts(accounts),
        ))
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
    let context = crate::context::ensure_dir(&daemon.data_dir, context_scope).map_err(|error| {
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

/// Records a new run and its worktree, with its thread row for a normal thread, in one
/// transaction. If that fails, removes the worktree again.
async fn record(
    daemon: &Arc<Daemon>,
    run_id: RunId,
    (fields, state): (RunFields, RunState),
    is_thread: bool,
    repo_path: &Path,
    created: &CreatedWorktree,
) -> Result<
    (
        wisp_store::Run,
        wisp_store::Worktree,
        Option<wisp_store::Thread>,
    ),
    ErrorObject,
> {
    let worktree_fields = WorktreeFields {
        repo_path: repo_path.to_string_lossy().into_owned(),
        path: created.path.to_string_lossy().into_owned(),
        branch: created.branch.clone(),
        base: created.base.clone(),
        git_dir: created.git_dir.to_string_lossy().into_owned(),
    };
    let scope = fields.project_id;
    let recorded = store(daemon, move |db| {
        if is_thread {
            db.create_thread_run(run_id.into(), scope, &fields, &state, &worktree_fields)
                .map(|(thread, run, worktree)| (run, worktree, Some(thread)))
        } else {
            db.create_run_with_worktree(run_id.into(), &fields, &state, &worktree_fields)
                .map(|(run, worktree)| (run, worktree, None))
        }
        .map_err(|e| store_error(&e))
    })
    .await;
    if recorded.is_err()
        && let Err(cleanup) = daemon
            .agents
            .worktrees
            .remove(repo_path, &created.path, &created.branch)
            .await
    {
        warn!(run = %run_id, %cleanup, "could not remove a worktree for a run that wasn't recorded");
    }
    recorded
}

/// `agent/start`: see the module documentation. Idempotent on the run id.
pub(crate) async fn start(
    daemon: Arc<Daemon>,
    params: AgentStartParams,
) -> Result<AgentRun, ErrorObject> {
    let AgentStartParams {
        run_id,
        project,
        prompt,
        account,
        ..
    } = params;
    let new = NewRun {
        run_id,
        scope: project,
        prompt,
        account,
        thread: None,
    };
    Ok(create(daemon, new).await?.run)
}

/// A run to create: a project's worker, or a normal thread (#110), which belongs to a repo entry
/// instead of a project.
pub(crate) struct NewRun {
    pub run_id: RunId,
    /// The project, or for a thread its repo entry, whose id the run's events go to.
    pub scope: ProjectId,
    pub prompt: String,
    pub account: Option<AccountChoice>,
    pub thread: Option<NewThread>,
}

/// What a normal thread adds to a run.
pub(crate) struct NewThread {
    /// A thread with no repo's own scratch repository, which the caller made. Its worktree is
    /// cut from this instead of from the scope's path.
    pub scratch: Option<PathBuf>,
}

/// A created run, and its thread row for a normal thread.
pub(crate) struct CreatedRun {
    pub run: AgentRun,
    pub thread: Option<wisp_store::Thread>,
}

/// Creates and starts a run: see the module documentation. Idempotent on the run id.
pub(crate) async fn create(daemon: Arc<Daemon>, new: NewRun) -> Result<CreatedRun, ErrorObject> {
    let agents = &daemon.agents;
    let NewRun {
        run_id,
        scope: project,
        prompt,
        account,
        thread,
    } = new;
    let _starting = agents.start_guard(run_id).await;
    let requested = requested_account(account.as_ref());

    if let Some(run) = existing(&daemon, run_id, project, &prompt, requested.as_deref()).await? {
        let row = if thread.is_some() {
            Some(crate::threads::existing_thread(&daemon, run_id).await?)
        } else {
            None
        };
        return Ok(CreatedRun { run, thread: row });
    }
    let (prepared, scope_path) = prepare(&daemon, project, run_id, account).await?;
    let repo_path = match thread.as_ref().and_then(|thread| thread.scratch.clone()) {
        Some(scratch) => scratch.to_string_lossy().into_owned(),
        None => scope_path,
    };
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
    let is_thread = thread.is_some();
    let (row, worktree, thread_row) = record(
        &daemon,
        run_id,
        (fields, state),
        is_thread,
        Path::new(&repo_path),
        &created,
    )
    .await?;
    let snapshot = agent_run(&row, Some(&worktree))?;
    daemon
        .log
        .append(
            snapshot.created_at,
            Some(project),
            WispEvent::AgentStarted {
                run_id,
                run: Some(snapshot),
            },
        )
        .await;
    if let Some(thread) = &thread_row {
        crate::threads::log_started(&daemon, thread).await;
    }
    info!(run = %run_id, project = %project, backend = %row.fields.backend, thread = is_thread, "created an agent run");

    // A run just created here has no sent turns yet.
    let mut actor = Actor::new(Arc::clone(&daemon), row, Some(worktree), HashMap::new());
    let task = match &thread {
        Some(thread) => worker::thread_prompt(
            &prompt,
            &worktree_path,
            &prepared.context,
            thread.scratch.is_some(),
        ),
        None => worker::worker_prompt(&prompt, &worktree_path, &prepared.context),
    };
    actor
        .launch(
            prepared,
            task,
            None,
            None,
            Some((worktree_path, git_common_dir)),
        )
        .await;
    // The actor owns a live CLI from here on, so it is spawned whatever the snapshot says.
    let run = actor.snapshot();
    agents.spawn(actor);
    Ok(CreatedRun {
        run: run?,
        thread: thread_row,
    })
}

impl Agents {
    /// Drops run `id`'s actor from the map, so no new command reaches it.
    pub(crate) fn forget(&self, id: RunId) {
        self.actors
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&id);
    }

    /// The worktrees every run is created in.
    pub(crate) fn worktrees(&self) -> &WorktreeManager {
        &self.worktrees
    }
}

/// The command channel of `id`'s actor, spawning one for a run created before this wispd
/// started.
async fn actor_for(daemon: &Arc<Daemon>, id: RunId) -> Result<mpsc::Sender<Command>, ErrorObject> {
    let agents = &daemon.agents;
    if let Some(actor) = agents.actor(id) {
        return Ok(actor);
    }
    let _starting = agents.start_guard(id).await;
    if let Some(actor) = agents.actor(id) {
        return Ok(actor);
    }
    let (row, worktree, turns) = store(daemon, move |db| {
        let row = db
            .get_run(id.into())
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| run_not_found(id))?;
        let worktree = db.get_worktree(id.into()).map_err(|e| store_error(&e))?;
        if worktree.is_none() && row.state.status != convert::ACCEPTED {
            return Err(ErrorObject::internal_error(format!(
                "run {id} has no recorded worktree"
            )));
        }
        let turns = db.run_turns(id.into()).map_err(|e| store_error(&e))?;
        Ok((row, worktree, turns))
    })
    .await?;
    // A restarted actor rebuilds `agent/send`'s idempotency from the store (#190), since a fresh
    // one has no memory of what a previous wispd already sent to this run's CLI.
    let turns = turns
        .into_iter()
        .filter_map(|(turn_id, text)| {
            if let Ok(turn_id) = TurnId::try_from(turn_id) {
                Some((turn_id, text))
            } else {
                warn!(run = %id, "a stored turn id is not a UUIDv7; ignoring it");
                None
            }
        })
        .collect();
    Ok(agents.spawn(Actor::new(Arc::clone(daemon), row, worktree, turns)))
}

async fn ask<T>(
    daemon: &Arc<Daemon>,
    id: RunId,
    command: impl FnOnce(oneshot::Sender<Result<T, ErrorObject>>) -> Command,
) -> Result<T, ErrorObject> {
    let (reply, answer) = oneshot::channel();
    let mut command = command(reply);
    let stopping = || ErrorObject::internal_error("wispd is stopping");
    // An actor that `thread/delete` just stopped has closed its channel: look the run up again,
    // which then finds it gone.
    for _ in 0..2 {
        let actor = actor_for(daemon, id).await?;
        match actor.send(command).await {
            Ok(()) => return answer.await.map_err(|_| stopping())?,
            Err(mpsc::error::SendError(returned)) => command = returned,
        }
    }
    Err(stopping())
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

/// `thread/delete`'s part in the runner: through the run's actor, which stops a running CLI
/// first and never races the run's own resume, commit, or accept.
pub(crate) async fn delete(daemon: &Arc<Daemon>, id: RunId) -> Result<(), ErrorObject> {
    ask(daemon, id, |reply| Command::Delete { reply }).await
}

/// `agent/accept`: through the run's actor, so it never races the run's own CLI or commit.
pub(crate) async fn accept(
    daemon: Arc<Daemon>,
    params: AgentAcceptParams,
) -> Result<AgentAcceptResult, ErrorObject> {
    let AgentAcceptParams { run_id, id, commit } = params;
    let (run, merge) = ask(&daemon, run_id, |reply| Command::Accept {
        id,
        reviewed: commit,
        reply,
    })
    .await?;
    Ok(AgentAcceptResult { run, merge })
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
                daemon
                    .log
                    .append(
                        run.updated_at,
                        Some(run.project),
                        WispEvent::AgentFinished {
                            run_id: run.id,
                            outcome: AgentOutcome::Interrupted,
                        },
                    )
                    .await;
                daemon
                    .log
                    .append(
                        run.updated_at,
                        Some(run.project),
                        WispEvent::AgentUpdated {
                            run_id: run.id,
                            state: AgentRunState {
                                status: run.status,
                                account_id: run.account_id,
                                session_id: run.session_id,
                                error: run.error,
                                diff: run.diff,
                                updated_at: run.updated_at,
                            },
                        },
                    )
                    .await;
            }
        }
        Err(error) => warn!(error = %error.message, "could not recover interrupted agent runs"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use wisp_protocol::RunId;

    use super::StartLocks;

    #[test]
    fn different_run_ids_get_independent_locks() {
        let locks = StartLocks::default();
        let (a, b) = (RunId::generate(), RunId::generate());
        assert!(!Arc::ptr_eq(&locks.get(a), &locks.get(b)));
    }

    #[test]
    fn the_same_run_id_gets_the_same_lock_until_it_is_released() {
        let locks = StartLocks::default();
        let id = RunId::generate();
        let first = locks.get(id);
        assert!(Arc::ptr_eq(&first, &locks.get(id)));
        locks.release(id);
        assert!(
            !Arc::ptr_eq(&first, &locks.get(id)),
            "a released id starts fresh, for the next caller to lock uncontended"
        );
    }

    /// #190 N4: two different runs proceed concurrently through `agents::start`/`actor_for`,
    /// while retries or a race for the very same run id still serialize, exactly as the single
    /// lock this replaced did.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_different_run_ids_proceed_concurrently_while_the_same_one_serializes() {
        let locks = StartLocks::default();
        let (a, b) = (RunId::generate(), RunId::generate());
        let hold_a = locks.get(a).lock_owned().await;

        tokio::time::timeout(Duration::from_millis(200), locks.get(b).lock_owned())
            .await
            .expect("a different run id was blocked by an unrelated one's lock");

        let waiting = tokio::spawn({
            let lock = locks.get(a);
            async move {
                lock.lock_owned().await;
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !waiting.is_finished(),
            "a retry for the same run id did not wait for its own lock"
        );
        drop(hold_a);
        waiting.await.unwrap();
    }

    /// #190 review, blocking item 3: releasing a run id's lock while a queued retry still holds a
    /// clone of it must not let a *third*, fresh caller in on a different, uncontended lock. That
    /// would mean the retry and the fresh caller could both end up inside the run's critical
    /// section at once — exactly what happens after a failed `agent/start`, since the failed
    /// attempt's fast path (`existing()`) has nothing to find, so a naive `release` looks safe to
    /// call unconditionally.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_released_lock_is_not_reused_while_a_queued_retry_still_holds_it() {
        let locks = Arc::new(StartLocks::default());
        let id = RunId::generate();

        // The first attempt takes the lock, then fails and drops its own guard.
        let first = locks.get(id);
        let first_guard = Arc::clone(&first).lock_owned().await;

        // A retry queues behind it, using the very same lock instance.
        let retry = locks.get(id);
        assert!(
            Arc::ptr_eq(&first, &retry),
            "a queued retry shares the first attempt's own lock"
        );
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let retry_task = tokio::spawn({
            let retry = Arc::clone(&retry);
            let entered = Arc::clone(&entered);
            async move {
                let _guard = retry.lock_owned().await;
                entered.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!retry_task.is_finished(), "the retry is still queued");

        // The first attempt "fails" (a real caller's `Starting` guard would drop here) and
        // releases, exactly as `agents::start`/`actor_for` do on any error path.
        drop(first_guard);
        locks.release(id);

        // A caller arriving after the release, while the retry is still queued, must still be
        // handed the SAME lock: nothing has succeeded yet, so there is no fast path (`agents.
        // actor(id)`/`existing()`) to protect a third caller from racing the retry.
        let fresh = locks.get(id);
        assert!(
            Arc::ptr_eq(&retry, &fresh),
            "a caller after the release still contends for the queued retry's own lock"
        );
        assert!(
            !entered.load(std::sync::atomic::Ordering::SeqCst),
            "the retry has not run yet: nothing has bypassed it"
        );

        retry_task.await.unwrap();
        assert!(entered.load(std::sync::atomic::Ordering::SeqCst));

        // Once nobody but the map itself holds it — dropping every local clone this test kept
        // around, not just the ones a real caller would have released already — the *next* `get`
        // sweeps it away and a later caller gets a brand-new, uncontended lock: the entry doesn't
        // leak forever. Checked with a `Weak` rather than comparing the new `Arc`'s address to
        // the old one's: once the old allocation is freed, a new one is free to reuse the very
        // same address, which would make a raw-pointer comparison an unreliable false negative.
        let old = Arc::downgrade(&retry);
        drop((first, retry, fresh));
        let after = locks.get(id);
        assert!(
            old.upgrade().is_none(),
            "the swept lock is still kept alive somewhere"
        );
        drop(after);
    }
}
