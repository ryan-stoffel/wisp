//! The M3 runner (#156, decision 0014): runs a worker end to end.
//!
//! `agent/start` resolves the worker's account through routing (#119), refuses a worker plxd
//! can't sandbox (0013) or a run option its backend can't honor (RYA-97),
//! creates the run's worktree (#154), records the run, and starts the
//! backend in the worktree with the project's shared context folder (#155) writable. From then
//! on one [`actor`] task per run owns it: it streams the backend's events into the event log as
//! `agent.*` events, records usage (#120) against whichever account the run is on, takes
//! `agent/send` and `agent/cancel`, and when a CLI process ends, commits the worktree through
//! #166's hardened `commit_all` and reports `agent.diffReady`.
//!
//! A run whose client started it with `approvals`, and a subagent of a coordinator that has them,
//! lets its CLI ask before a tool call (RYA-222, decision 0031). It logs the request, takes
//! `agent/approve`'s answer, and denies it itself when nobody answers in time ([`approvals`]).
//! Every launch of the run, a resume included, keeps the flag.
//!
//! A run outlives its CLI processes: `agent/send` to a run whose CLI has ended resumes the
//! vendor session in the same worktree. When plxd stops, running CLIs are cancelled and their
//! runs recorded `interrupted`; a run still `starting` or `running` in the store when plxd
//! starts (a crash) is marked `interrupted` too. Either kind resumes through `agent/send`, and
//! either wakes the coordinator that started it once plxd starts again ([`wake::catch_up`]).
//!
//! A project's coordinator (0024) is a run too, started by [`coordinator::start`] instead, with
//! no recorded worktree; the same actor runs it. Runs it started wake it when they finish
//! ([`wake`]).
//!
//! A normal thread's run is full Claude Code in every mode, with no worker sandbox, when its client
//! answers permission requests, and its first message is the user's own (0034). A thread started
//! with `checkout` has no worktree either: it runs in its repo entry's own checkout, on the branch
//! the user has out or the one `checkoutRef` switches it to first. plxd never commits it, since the
//! checkout can hold the user's own uncommitted work, so its changes stay there for the user to
//! review, and it has no diff to accept or open a PR from.

mod actor;
mod approvals;
pub(crate) mod attached;
pub(crate) mod convert;
pub(crate) mod coordinator;
mod resume;
pub(crate) mod review;
mod wake;
pub(crate) mod worker;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::{
    AccountChoice, AgentAcceptParams, AgentAcceptResult, AgentApproveParams, AgentApproveResult,
    AgentDelivery, AgentEffort, AgentImageParams, AgentOpenPrResult, AgentOutcome, AgentPermission,
    AgentRun, AgentRunState, AgentSendParams, AgentStartParams, ApprovalId, CoordinatorThreadId,
    ErrorKind, GitStatus, ImageMediaType, ParallaxEvent, PrActParams, PrDiffResult, PrViewParams,
    ProjectId, PromptImage, PullRequest, QueueResult, Role, RunId, TurnId,
};
use parallax_store::{RunFields, RunState, StoreError, ThreadFields, WorktreeFields};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{error, info, warn};
use uuid::Uuid;

use self::actor::{Actor, Command};
pub(crate) use self::actor::{GitAction, QueueOp};
pub(crate) use self::approvals::APPROVAL_TIMEOUT;
pub(crate) use self::convert::agent_run as snapshot;
use self::convert::{RUNNING, STARTING, WORKSPACE_WRITE, agent_run, option_name, option_value};
pub(crate) use self::resume::Timing as ResumeTiming;
use self::worker::{StoredKeyAccounts, sandbox_path, worker_unavailable};
use crate::backend::{Backend, ToolPolicy, check_argument, codex, cursor};
use crate::routing::{self, BackendRegistry, Defaults, Resolved, RoutingError};
use crate::server::Daemon;
use crate::worktree::{CreatedWorktree, PrError, WorktreeError, WorktreeManager};

/// How long a stopping plxd waits for its runs to record that they were interrupted.
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
    /// How long a run's permission request waits for an answer (RYA-222).
    approval_timeout: Duration,
    /// When a run a usage limit stopped resumes (PLX-371).
    resume_timing: ResumeTiming,
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

/// What starting a run's CLI needs, from [`prepare`].
pub(super) struct Prepared {
    resolved: Resolved,
    accounts: StoredKeyAccounts,
    place: Place,
}

/// Where a run's CLI starts, and what that needs.
pub(super) enum Place {
    /// A worker, in its worktree, inside the worker sandbox (0013), which needs these folders.
    /// A normal thread is placed the same way (`thread`), and with `approvals` runs as full
    /// Claude Code instead (0034).
    Worker {
        home: PathBuf,
        data_dir: PathBuf,
        context: PathBuf,
        thread: bool,
    },
    /// A project's coordinator, with no sandbox (0024), in a detached worktree of `repo` that the
    /// actor refreshes before each CLI process (RYA-171).
    Coordinator { repo: PathBuf },
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
            approval_timeout: APPROVAL_TIMEOUT,
            resume_timing: ResumeTiming::default(),
        }
    }

    /// Denies a permission request nobody answered after `timeout` instead of
    /// [`APPROVAL_TIMEOUT`].
    #[must_use]
    pub fn with_approval_timeout(mut self, timeout: Duration) -> Self {
        self.approval_timeout = timeout;
        self
    }

    /// How long a permission request waits for an answer.
    pub(super) fn approval_timeout(&self) -> Duration {
        self.approval_timeout
    }

    /// Resumes runs a usage limit stopped with `timing` instead of the default.
    #[must_use]
    pub fn with_resume_timing(mut self, timing: ResumeTiming) -> Self {
        self.resume_timing = timing;
        self
    }

    /// When a run a usage limit stopped resumes.
    pub(super) fn resume_timing(&self) -> ResumeTiming {
        self.resume_timing
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
    /// interrupted. Called once, when plxd stops, before the store closes.
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
    job: impl FnOnce(&mut parallax_store::Store) -> Result<T, ErrorObject> + Send + 'static,
) -> Result<T, ErrorObject> {
    daemon.store.run(&CancellationToken::new(), job).await
}

pub(crate) fn store_error(error: &StoreError) -> ErrorObject {
    error!(%error, "the project store failed for an agent run");
    ErrorObject::internal_error(format!("the project store failed: {error}"))
}

pub(crate) fn run_not_found(id: RunId) -> ErrorObject {
    ErrorObject::parallax(ErrorKind::RunNotFound, format!("no agent run has id {id}"))
}

pub(crate) fn run_accepted(id: RunId) -> ErrorObject {
    ErrorObject::parallax(
        ErrorKind::RunAccepted,
        format!("run {id} was accepted; its worktree and branch are gone"),
    )
}

fn requested_account(account: Option<&AccountChoice>) -> Option<String> {
    account.and_then(|account| serde_json::to_string(account).ok())
}

/// The project's repository, the routing inputs, and the paths run `run` of `project` needs,
/// checked: everything that can refuse a worker, or a coordinator when `role` is one, before
/// anything is created. For a run that exists already, or one that isn't a thread.
pub(super) async fn prepare(
    daemon: &Arc<Daemon>,
    project: ProjectId,
    run: RunId,
    requested: Option<AccountChoice>,
    role: Role,
) -> Result<(Prepared, String), ErrorObject> {
    prepare_run(daemon, project, run, requested, role, false).await
}

/// [`prepare`], where `new_thread` says the run being created is a normal thread's
/// (`thread/start`), whose thread row doesn't exist yet.
async fn prepare_run(
    daemon: &Arc<Daemon>,
    project: ProjectId,
    run: RunId,
    requested: Option<AccountChoice>,
    role: Role,
    new_thread: bool,
) -> Result<(Prepared, String), ErrorObject> {
    let (repo_path, context_scope, thread, thread_run, defaults, accounts) =
        store(daemon, move |db| {
            let repo_path = crate::threads::scope_path(db, project)?;
            // A run whose scope is a repo entry is a normal thread's (0017).
            let thread = db
                .get_repo(project.into())
                .map_err(|e| store_error(&e))?
                .is_some();
            // The run itself is a thread: one `thread/start` made, not any run on a repo entry,
            // such as a coordinator's worker started on one.
            let thread_run = new_thread
                || db
                    .get_thread(run.into())
                    .map_err(|e| store_error(&e))?
                    .is_some();
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
                thread,
                thread_run,
                defaults,
                StoredKeyAccounts(accounts),
            ))
        })
        .await?;
    let defaults = Defaults {
        coordinator: defaults.coordinator,
        worker: defaults.worker,
    };
    let policy = match role {
        Role::Coordinator => ToolPolicy::NoWrite,
        Role::Worker => ToolPolicy::WorkspaceWrite,
    };
    let resolved = routing::resolve(
        &daemon.agents.backends,
        &accounts,
        &defaults,
        role,
        requested,
        policy,
    )
    .map_err(|error| routing_error(&error))?;
    if role == Role::Coordinator {
        coordinator::check_backend(resolved.backend())?;
        let place = Place::Coordinator {
            repo: PathBuf::from(&repo_path),
        };
        let prepared = Prepared {
            resolved,
            accounts,
            place,
        };
        return Ok((prepared, repo_path));
    }
    // A Codex or Cursor thread is full Codex or Cursor Agent, with no worker sandbox to check
    // (0035, 0036). Any other run on them is refused, even one on a repo entry: Cursor runs
    // nothing else, and Codex workers wait on RYA-153.
    let full = [codex::PROGRAM, cursor::NAME].contains(&resolved.backend().name());
    if !(thread_run && full) {
        worker::check_backend(resolved.backend())?;
    }
    if let Some(cli) = worker::cli_of(resolved.backend()) {
        // Only this CLI's status: a full probe also waits on the slowest of the others.
        let mut detected = daemon.cli_detector.get(cli).await;
        if worker::check_version(cli, Some(&detected)).is_err() {
            // The user may have just updated the CLI: look again before refusing.
            detected = daemon.cli_detector.refresh_one(cli).await;
            worker::check_version(cli, Some(&detected))?;
        }
        #[cfg(target_os = "linux")]
        if cli == parallax_protocol::CliKind::Claude {
            worker::check_linux_sandbox(&daemon.cli_detector, Some(&detected)).await?;
        }
    }
    let home = worker::home()?;
    let data_dir = sandbox_path(daemon.data_dir.root(), "plxd's data folder")?;
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
        place: Place::Worker {
            home,
            data_dir,
            context,
            thread,
        },
    };
    Ok((prepared, repo_path))
}

/// What a run asks of its CLI beyond the prompt (RYA-97), each `None` for the CLI's default. The
/// run keeps them for every launch, including a resume. A waiting message stores its own as JSON
/// (PLX-370).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(crate) struct RunOptions {
    pub model: Option<String>,
    pub effort: Option<AgentEffort>,
    pub permission: Option<AgentPermission>,
    pub context_window: Option<u32>,
    pub fast: Option<bool>,
}

impl RunOptions {
    /// Refuses, with `unsupportedOption`, a model name that can't be a CLI argument, or an
    /// effort, permission, context window, or fast mode that `backend` doesn't map.
    fn check(&self, backend: &dyn Backend) -> Result<(), ErrorObject> {
        let refuse = |detail: String| ErrorObject::parallax(ErrorKind::UnsupportedOption, detail);
        let name = backend.name();
        if let Some(model) = &self.model
            && check_argument("model", model).is_err()
        {
            return Err(refuse(format!(
                "the model {model:?} can't be passed to the {name} backend's CLI"
            )));
        }
        if let Some(effort) = self.effort
            && !backend.efforts().contains(&effort)
        {
            let effort = option_name(effort).unwrap_or_default();
            return Err(refuse(format!(
                "the {name} backend can't run with effort {effort}"
            )));
        }
        if let Some(permission) = self.permission
            && !backend.permissions().contains(&permission)
        {
            let permission = option_name(permission).unwrap_or_default();
            return Err(refuse(format!(
                "the {name} backend can't run with permission {permission}"
            )));
        }
        if let Some(tokens) = self.context_window
            && !backend.context_windows().contains(&tokens)
        {
            return Err(refuse(format!(
                "the {name} backend can't run with a {tokens}-token context window"
            )));
        }
        if self.fast.is_some() && !backend.fast_mode() {
            return Err(refuse(format!(
                "the {name} backend can't run in or out of fast mode"
            )));
        }
        Ok(())
    }
}

fn routing_error(error: &RoutingError) -> ErrorObject {
    match error {
        &RoutingError::NoAccount { role } => ErrorObject::parallax(
            ErrorKind::NoDefaultAccount,
            format!(
                "no account was named, and the {} role has no default; set one with \
                 accounts/defaults/set",
                crate::store::role_text(role)
            ),
        ),
        &RoutingError::UnknownKeyAccount { id } => ErrorObject::parallax(
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

/// Switches the checkout at `repo_path`, `project`'s (a thread's repo entry), to `reference`.
/// Refuses, with `worktreeFailed`, while another thread runs in it, since the switch would move
/// that thread's work to a branch it never chose.
async fn switch_checkout(
    daemon: &Daemon,
    project: ProjectId,
    repo_path: &Path,
    reference: &str,
) -> Result<(), ErrorObject> {
    let busy = store(daemon, move |db| {
        db.list_runs(Some(project.into()))
            .map(|runs| {
                runs.iter().any(|run| {
                    run.fields.checkout && [STARTING, RUNNING].contains(&run.state.status.as_str())
                })
            })
            .map_err(|e| store_error(&e))
    })
    .await?;
    if busy {
        return Err(ErrorObject::parallax(
            ErrorKind::WorktreeFailed,
            "another thread is running in this checkout; switching its branch would move that \
             thread's work",
        ));
    }
    daemon
        .agents
        .worktrees
        .switch(repo_path, reference)
        .await
        .map_err(|error| worktree_failed(&error))
}

fn worktree_failed(error: &WorktreeError) -> ErrorObject {
    ErrorObject::parallax(ErrorKind::WorktreeFailed, error.to_string())
}

/// The run `run_id` already is, for a retry of `agent/start` that asks for the same `fields`, or
/// `idConflict` if they differ. The backend isn't compared: routing resolves it, not the
/// request, and neither is a parent the store no longer has. `None` for a new run.
async fn existing(
    daemon: &Arc<Daemon>,
    run_id: RunId,
    fields: &RunFields,
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
    // An empty stored parent isn't compared: deleting the parent cleared it (0041), and a retry
    // still names it.
    let stored = RunFields {
        backend: fields.backend.clone(),
        parent: row.fields.parent.or(fields.parent),
        ..row.fields.clone()
    };
    if stored != *fields {
        return Err(ErrorObject::parallax(
            ErrorKind::IdConflict,
            format!(
                "run {run_id} exists with a different project, prompt, account, policy, \
                 coordinator thread, parent, model, effort, permission, context window, fast mode, \
                 or approvals"
            ),
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
    thread: Option<&NewThread>,
) -> Result<(CreatedWorktree, PathBuf, PathBuf), ErrorObject> {
    let branch_slug = thread.and_then(|thread| thread.branch_slug.as_deref());
    let base = thread.and_then(|thread| thread.git_ref.as_deref());
    let created = agents
        .worktrees
        .create_named(repo_path, run_id, base, branch_slug)
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

/// The canonical paths a run in `repo_path`'s own checkout needs, checked for the sandbox: the
/// checkout, its cwd, and the repository's shared git folder, which stays read-only to it.
pub(super) async fn checkout_paths(
    agents: &Agents,
    repo_path: &Path,
) -> Result<(PathBuf, PathBuf), ErrorObject> {
    let cwd = sandbox_path(repo_path, "the project's repository")?;
    let common = agents
        .worktrees
        .git_common_dir(repo_path)
        .await
        .map_err(|error| worktree_failed(&error))?;
    let common = sandbox_path(&common, "the repository's git folder")?;
    Ok((cwd, common))
}

/// Records a new run and its worktree, with its thread row for a normal thread (`thread` is
/// `Some`), in one transaction. If that fails, removes the worktree again. A thread in the
/// current checkout has no worktree (`created` is `None`); any other run must have one.
async fn record(
    daemon: &Arc<Daemon>,
    run_id: RunId,
    (fields, state): (RunFields, RunState),
    thread: Option<ThreadFields>,
    repo_path: &Path,
    created: Option<&CreatedWorktree>,
) -> Result<
    (
        parallax_store::Run,
        Option<parallax_store::Worktree>,
        Option<parallax_store::Thread>,
    ),
    ErrorObject,
> {
    let worktree_fields = created.map(|created| WorktreeFields {
        repo_path: repo_path.to_string_lossy().into_owned(),
        path: created.path.to_string_lossy().into_owned(),
        branch: created.branch.clone(),
        base: created.base.clone(),
        git_dir: created.git_dir.to_string_lossy().into_owned(),
        base_dirty: created.base_dirty,
    });
    let scope = fields.project_id;
    let recorded = store(daemon, move |db| {
        // `prepare_run` read the scope in an earlier job: a Project that `project/delete` removed
        // since gets no run (PLX-338).
        if db
            .get_project(scope)
            .map_err(|e| store_error(&e))?
            .is_none()
            && db.get_repo(scope).map_err(|e| store_error(&e))?.is_none()
        {
            return Err(ErrorObject::parallax(
                ErrorKind::ProjectNotFound,
                format!("no project has id {scope}"),
            ));
        }
        if let Some(thread) = &thread {
            db.create_thread_run(
                run_id.into(),
                scope,
                &fields,
                &state,
                worktree_fields.as_ref(),
                thread,
            )
            .map(|(thread, run, worktree)| (run, worktree, Some(thread)))
            .map_err(|e| store_error(&e))
        } else {
            let Some(worktree_fields) = worktree_fields else {
                return Err(ErrorObject::internal_error("a worker has no worktree"));
            };
            db.create_run_with_worktree(run_id.into(), &fields, &state, &worktree_fields)
                .map(|(run, worktree)| (run, Some(worktree), None))
                .map_err(|e| store_error(&e))
        }
    })
    .await;
    if recorded.is_err()
        && let Some(created) = created
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

/// A coordinator's subagent runs in the coordinator's current permission mode unless it names its
/// own (0027), and forwards its permission requests when the coordinator does (0031): sets
/// `options`' permission to the mode of the coordinator whose thread is `coordinator_thread`, its
/// own run (0024), sets `approvals` when that run has them, and returns the inherited mode. The
/// coordinator's mode only changes between its turns, and its `approvals` never do, so a retried
/// spawn from the same turn inherits the same.
// ponytail: `create` drops an inherited mode the subagent's backend lacks, so a retry of that
// spawn gets idConflict; keep requested and inherited modes apart if that bites.
async fn inherit(
    daemon: &Arc<Daemon>,
    coordinator_thread: Option<CoordinatorThreadId>,
    options: &mut RunOptions,
    approvals: &mut bool,
) -> Result<Option<AgentPermission>, ErrorObject> {
    let Some(thread) = coordinator_thread else {
        return Ok(None);
    };
    let id = Uuid::from(thread);
    let Some(row) = store(daemon, move |db| {
        db.get_run(id).map_err(|e| store_error(&e))
    })
    .await?
    else {
        return Ok(None);
    };
    *approvals |= row.fields.approvals;
    if options.permission.is_some() {
        return Ok(None);
    }
    options.permission = row.fields.permission.as_deref().and_then(option_value);
    Ok(options.permission)
}

/// Logs `run`'s `agent.started` on `project`'s events.
async fn log_started(daemon: &Daemon, project: ProjectId, run: AgentRun) {
    let event = ParallaxEvent::AgentStarted {
        run_id: run.id,
        run: Some(run.clone()),
    };
    daemon
        .log
        .append(run.created_at, Some(project), event)
        .await;
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
        coordinator_thread,
        model,
        effort,
        permission,
        context_window,
        fast,
        images,
        approvals,
        threads,
        ..
    } = params;
    let new = NewRun {
        run_id,
        scope: project,
        prompt,
        images,
        threads,
        account,
        coordinator_thread,
        options: RunOptions {
            model,
            effort,
            permission,
            context_window,
            fast,
        },
        approvals,
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
    /// The prompt's images (RYA-191), already checked.
    pub images: Vec<PromptImage>,
    /// The threads attached to the prompt (PLX-372), already checked.
    pub threads: Vec<RunId>,
    pub account: Option<AccountChoice>,
    /// The coordinator thread starting the run through `plxd mcp` (#195).
    pub coordinator_thread: Option<CoordinatorThreadId>,
    pub options: RunOptions,
    /// The client answers the run's permission requests (RYA-222, 0031).
    pub approvals: bool,
    pub thread: Option<NewThread>,
}

/// What a normal thread adds to a run.
pub(crate) struct NewThread {
    /// A thread with no repo's own scratch repository, which the caller made. Its worktree is
    /// cut from this instead of from the scope's path.
    pub scratch: Option<PathBuf>,
    /// The name after `parallax/` for the worktree's branch, already checked.
    pub branch_slug: Option<String>,
    /// Work in the repo entry's own checkout instead of a worktree. Never set with `scratch`.
    pub checkout: bool,
    /// The ref the worktree starts from, or with `checkout`, the ref the checkout switches to
    /// first. Already checked.
    pub git_ref: Option<String>,
    /// The run that launches it (0041), already checked to exist.
    pub parent: Option<RunId>,
    /// Its fork origin and title (0041), already checked.
    pub fields: ThreadFields,
}

/// A created run, and its thread row for a normal thread.
pub(crate) struct CreatedRun {
    pub run: AgentRun,
    pub thread: Option<parallax_store::Thread>,
}

/// A new run's parent (0041): the one its thread names, or the coordinator that starts it, whose
/// subagents are its children.
fn parent(thread: Option<&NewThread>, coordinator: Option<CoordinatorThreadId>) -> Option<Uuid> {
    thread
        .and_then(|thread| thread.parent)
        .map(Uuid::from)
        .or(coordinator.map(Uuid::from))
}

/// Creates and starts a run: see the module documentation. Idempotent on the run id.
#[expect(
    clippy::too_many_lines,
    reason = "one sequence of steps, each of which must happen before the next"
)]
pub(crate) async fn create(daemon: Arc<Daemon>, new: NewRun) -> Result<CreatedRun, ErrorObject> {
    let agents = &daemon.agents;
    let NewRun {
        run_id,
        scope: project,
        prompt,
        images,
        threads,
        account,
        coordinator_thread,
        mut options,
        mut approvals,
        thread,
    } = new;
    let _starting = agents.start_guard(run_id).await;
    let inherited = inherit(&daemon, coordinator_thread, &mut options, &mut approvals).await?;
    // What the request asks for, as the runs table stores it. Routing fills in the backend below.
    let mut fields = RunFields {
        project_id: project.into(),
        prompt: prompt.clone(),
        requested_account: requested_account(account.as_ref()),
        policy: WORKSPACE_WRITE.to_owned(),
        backend: String::new(),
        coordinator_thread: coordinator_thread.map(Uuid::from),
        parent: parent(thread.as_ref(), coordinator_thread),
        model: options.model.clone(),
        effort: options.effort.and_then(option_name),
        permission: options.permission.and_then(option_name),
        context_window: options.context_window,
        fast: options.fast,
        approvals,
        checkout: thread.as_ref().is_some_and(|thread| thread.checkout),
    };

    if let Some(run) = existing(&daemon, run_id, &fields).await? {
        let row = if thread.is_some() {
            Some(crate::threads::existing_thread(&daemon, run_id).await?)
        } else {
            None
        };
        return Ok(CreatedRun { run, thread: row });
    }
    // The attached threads are read before anything is created, so a failure leaves nothing.
    let sent = attached::prompt(&daemon, &threads, &prompt).await?;
    let (prepared, scope_path) = prepare_run(
        &daemon,
        project,
        run_id,
        account,
        Role::Worker,
        thread.is_some(),
    )
    .await?;
    if inherited.is_some_and(|mode| !prepared.resolved.backend().permissions().contains(&mode)) {
        (options.permission, fields.permission) = (None, None);
    }
    options.check(prepared.resolved.backend())?;
    let repo_path = match thread.as_ref().and_then(|thread| thread.scratch.clone()) {
        Some(scratch) => scratch.to_string_lossy().into_owned(),
        None => scope_path,
    };
    let (created, (cwd, git_common_dir)) = if fields.checkout {
        if let Some(reference) = thread.as_ref().and_then(|thread| thread.git_ref.as_deref()) {
            switch_checkout(&daemon, project, Path::new(&repo_path), reference).await?;
        }
        (None, checkout_paths(agents, Path::new(&repo_path)).await?)
    } else {
        let (created, worktree_path, git_common_dir) =
            create_worktree(agents, Path::new(&repo_path), run_id, thread.as_ref()).await?;
        (Some(created), (worktree_path, git_common_dir))
    };

    fields.backend = prepared.resolved.backend().name().into();
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
        thread.map(|thread| thread.fields),
        Path::new(&repo_path),
        created.as_ref(),
    )
    .await?;
    log_started(&daemon, project, agent_run(&row, worktree.as_ref())?).await;
    if let Some(thread) = &thread_row {
        crate::threads::log_started(&daemon, thread).await;
    }
    info!(run = %run_id, project = %project, backend = %row.fields.backend, thread = is_thread, checkout = row.fields.checkout, "created an agent run");

    // A run just created here has no sent turns yet.
    let mut actor = Actor::new(Arc::clone(&daemon), row, worktree, HashMap::new());
    let task = first_prompt(&sent, &prepared.place, &cwd)?;
    let paths = Some((cwd, git_common_dir));
    actor.attach(None, threads);
    actor
        .launch(prepared, task, images, None, None, paths)
        .await;
    // The actor owns a live CLI from here on, so it is spawned whatever the snapshot says.
    let run = actor.snapshot();
    agents.spawn(actor);
    Ok(CreatedRun {
        run: run?,
        thread: thread_row,
    })
}

/// A new run's first message: a worker's limits, for its CLI started in `cwd`, then `prompt`. A
/// thread's (a run whose scope is a repo entry) is `prompt` as the user wrote it, as in Claude
/// Code (0034).
fn first_prompt(prompt: &str, place: &Place, cwd: &Path) -> Result<String, ErrorObject> {
    match place {
        Place::Worker { thread: true, .. } => Ok(prompt.to_owned()),
        Place::Worker { context, .. } => Ok(worker::worker_prompt(prompt, cwd, context)),
        Place::Coordinator { .. } => Err(ErrorObject::internal_error(
            "a worker was prepared as a coordinator",
        )),
    }
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

    /// The backends runs start on.
    pub(crate) fn backends(&self) -> &BackendRegistry {
        &self.backends
    }
}

/// The command channel of `id`'s actor, spawning one for a run created before this plxd
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
        // Only an accepted run has lost its worktree, and a coordinator or a checkout thread
        // never had one (0024).
        if worktree.is_none()
            && row.state.status != convert::ACCEPTED
            && row.fields.policy != convert::NO_WRITE
            && !row.fields.checkout
        {
            return Err(ErrorObject::internal_error(format!(
                "run {id} has no recorded worktree"
            )));
        }
        let turns = db.run_turns(id.into()).map_err(|e| store_error(&e))?;
        Ok((row, worktree, turns))
    })
    .await?;
    // A restarted actor rebuilds `agent/send`'s idempotency from the store (#190), since a fresh
    // one has no memory of what a previous plxd already sent to this run's CLI.
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
    let stopping = || ErrorObject::internal_error("plxd is stopping");
    // An actor that `thread/delete` or `project/delete` just stopped has closed its channel: look
    // the run up again, which then finds it gone.
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
        model,
        effort,
        permission,
        context_window,
        fast,
        account,
        images,
        threads,
        delivery,
    } = params;
    let steer = match delivery.unwrap_or_default() {
        AgentDelivery::Queue => false,
        AgentDelivery::Steer => true,
        AgentDelivery::Unknown => {
            return Err(ErrorObject::invalid_params(
                "delivery must be queue or steer",
            ));
        }
    };
    let options = RunOptions {
        model,
        effort,
        permission,
        context_window,
        fast,
    };
    ask(&daemon, run_id, |reply| Command::Send {
        turn_id,
        text,
        images,
        threads,
        options,
        account,
        steer,
        reply,
    })
    .await
}

/// `queue/*` (PLX-370): through the run's actor, which keeps its waiting messages.
pub(crate) async fn queue(
    daemon: Arc<Daemon>,
    run_id: RunId,
    op: QueueOp,
) -> Result<QueueResult, ErrorObject> {
    ask(&daemon, run_id, |reply| Command::Queue { op, reply }).await
}

/// Starts the actor of every run that has waiting messages a plxd before this one stored, so
/// they are sent (PLX-370). Called once at startup, after [`recover`].
pub(crate) async fn deliver_queued(daemon: &Arc<Daemon>) {
    let runs = store(daemon, |db| db.queued_runs().map_err(|e| store_error(&e))).await;
    let runs = match runs {
        Ok(runs) => runs,
        Err(error) => {
            warn!(error = %error.message, "could not read which runs have waiting messages");
            return;
        }
    };
    for run in runs {
        let Ok(id) = RunId::try_from(run) else {
            warn!(%run, "a run with waiting messages has an id that is not a UUIDv7");
            continue;
        };
        if let Err(error) = actor_for(daemon, id).await {
            warn!(run = %id, error = %error.message, "could not send a run's waiting messages");
        }
    }
}

/// `agent/image`: one of a run's stored images (RYA-191, decision 0026).
pub(crate) async fn image(
    daemon: &Arc<Daemon>,
    params: AgentImageParams,
) -> Result<PromptImage, ErrorObject> {
    let AgentImageParams { run_id, image_id } = params;
    let stored = store(daemon, move |db| {
        if db
            .get_run(run_id.into())
            .map_err(|e| store_error(&e))?
            .is_none()
        {
            return Err(run_not_found(run_id));
        }
        db.image(run_id.into(), image_id.into())
            .map_err(|e| store_error(&e))
    })
    .await?
    .ok_or_else(|| {
        ErrorObject::parallax(
            ErrorKind::ImageNotFound,
            format!("run {run_id} has no image {image_id}"),
        )
    })?;
    Ok(PromptImage {
        media_type: option_value(&stored.media_type).unwrap_or(ImageMediaType::Unknown),
        data: stored.data,
    })
}

/// `agent/cancel`.
pub(crate) async fn cancel(daemon: Arc<Daemon>, id: RunId) -> Result<AgentRun, ErrorObject> {
    ask(&daemon, id, |reply| Command::Cancel { reply }).await
}

/// `agent/resumeNow` (PLX-371): resumes a run waiting for its usage limit to reset now.
pub(crate) async fn resume_now(daemon: Arc<Daemon>, id: RunId) -> Result<AgentRun, ErrorObject> {
    ask(&daemon, id, |reply| Command::ResumeNow { reply }).await
}

/// `agent/autoResume` (PLX-371): sets or clears a run's auto-resume override.
pub(crate) async fn set_auto_resume(
    daemon: Arc<Daemon>,
    id: RunId,
    auto_resume: Option<bool>,
) -> Result<AgentRun, ErrorObject> {
    ask(&daemon, id, |reply| Command::AutoResume {
        auto_resume,
        reply,
    })
    .await
}

/// `agent/approve` (RYA-222): through the run's actor, which keeps its permission requests. The
/// caller has checked `params`.
pub(crate) async fn approve(
    daemon: Arc<Daemon>,
    params: AgentApproveParams,
) -> Result<AgentApproveResult, ErrorObject> {
    let run_id = params.run_id;
    ask(&daemon, run_id, |reply| Command::Approve { params, reply }).await
}

pub(super) fn approval_not_found(run: RunId, approval: ApprovalId) -> ErrorObject {
    ErrorObject::parallax(
        ErrorKind::ApprovalNotFound,
        format!("run {run} has no permission request {approval}"),
    )
}

/// `thread/delete`'s and `project/delete`'s part in the runner: deletes a run through its actor,
/// which stops a running CLI first and never races the run's own resume, commit, or accept.
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

/// `agent/openPr`: through the run's actor, as `agent/accept` is (RYA-168).
pub(crate) async fn open_pr(
    daemon: Arc<Daemon>,
    run_id: RunId,
    title: String,
    body: String,
) -> Result<AgentOpenPrResult, ErrorObject> {
    let url = ask(&daemon, run_id, |reply| Command::OpenPr {
        title,
        body,
        reply,
    })
    .await?;
    Ok(AgentOpenPrResult { url })
}

/// `pr/view` (PLX-318): one of a run's linked pull requests, read with `gh`. Not through the
/// run's actor, so a slow GitHub never holds up a running agent.
pub(crate) async fn view_pr(
    daemon: Arc<Daemon>,
    params: PrViewParams,
) -> Result<PullRequest, ErrorObject> {
    let PrViewParams { run_id, url } = params;
    linked(&daemon, run_id, &url).await?;
    daemon
        .agents
        .worktrees
        .view_pr(&url)
        .await
        .map_err(|error| pr_error(&error))
}

/// `pr/diff` (PLX-328): one of a run's linked pull requests' unified diff, read with `gh` as
/// `pr/view` reads one.
pub(crate) async fn diff_pr(
    daemon: Arc<Daemon>,
    params: PrViewParams,
) -> Result<PrDiffResult, ErrorObject> {
    let PrViewParams { run_id, url } = params;
    linked(&daemon, run_id, &url).await?;
    daemon
        .agents
        .worktrees
        .diff_pr(&url)
        .await
        .map_err(|error| pr_error(&error))
}

/// `pr/act` (PLX-318): does an action to one of a run's linked pull requests with `gh`, as
/// `pr/view` reads one.
pub(crate) async fn act_pr(
    daemon: Arc<Daemon>,
    params: PrActParams,
) -> Result<PullRequest, ErrorObject> {
    let PrActParams {
        run_id,
        url,
        action,
    } = params;
    linked(&daemon, run_id, &url).await?;
    let acted = daemon
        .agents
        .worktrees
        .act_pr(&url, action)
        .await
        .map_err(|error| pr_error(&error))?;
    info!(run = %run_id, %url, ?action, "acted on a pull request");
    Ok(acted)
}

/// Refuses `url` unless it is linked to run `run_id`, so a client can't make plxd run `gh` on
/// any other argument.
async fn linked(daemon: &Daemon, run_id: RunId, url: &str) -> Result<(), ErrorObject> {
    let url = url.to_owned();
    store(daemon, move |db| {
        let row = db
            .get_run(run_id.into())
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| run_not_found(run_id))?;
        if row.state.pull_requests.contains(&url) {
            Ok(())
        } else {
            Err(ErrorObject::invalid_params(format!(
                "{url} is not a pull request linked to run {run_id}"
            )))
        }
    })
    .await
}

/// A failed `gh` or push as the protocol's error.
pub(super) fn pr_error(error: &PrError) -> ErrorObject {
    let kind = match error {
        PrError::Push(_) => ErrorKind::PushFailed,
        PrError::GhUnavailable(_) => ErrorKind::GhUnavailable,
        PrError::Gh(_) => ErrorKind::PrFailed,
    };
    ErrorObject::parallax(kind, error.to_string())
}

/// `agent/gitStatus`, `agent/commit`, and `agent/push`: through the run's actor (RYA-298).
pub(crate) async fn git(
    daemon: Arc<Daemon>,
    run_id: RunId,
    action: GitAction,
) -> Result<GitStatus, ErrorObject> {
    ask(&daemon, run_id, |reply| Command::Git { action, reply }).await
}

/// Marks every run the store still has as `starting` or `running` as `interrupted`: plxd
/// stopped without recording how they ended, as after a crash. Then wakes each project's
/// coordinator for what it missed while plxd was stopped ([`wake::catch_up`], RYA-178), and
/// starts the timers of runs waiting for a usage limit to reset ([`resume::restore`], PLX-371).
/// Called once at startup, before any connection is accepted.
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
                info!(run = %run.id, "an agent run was interrupted when plxd last stopped");
                daemon
                    .log
                    .append(
                        run.updated_at,
                        Some(run.project),
                        ParallaxEvent::AgentFinished {
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
                        ParallaxEvent::AgentUpdated {
                            run_id: run.id,
                            state: AgentRunState {
                                status: run.status,
                                account_id: run.account_id,
                                backend: Some(run.backend),
                                session_id: run.session_id,
                                error: run.error,
                                diff: run.diff,
                                model: run.model,
                                effort: run.effort,
                                permission: run.permission,
                                context_window: run.context_window,
                                fast: run.fast,
                                pull_requests: run.pull_requests,
                                resume_at: run.resume_at,
                                auto_resume: run.auto_resume,
                                updated_at: run.updated_at,
                            },
                        },
                    )
                    .await;
            }
        }
        Err(error) => warn!(error = %error.message, "could not recover interrupted agent runs"),
    }
    wake::catch_up(daemon).await;
    resume::restore(daemon).await;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use std::path::Path;

    use parallax_protocol::{ErrorKind, RunId};
    use parallax_store::{ProjectFields, RunFields, RunState};
    use uuid::Uuid;

    use super::{StartLocks, record, store, store_error};
    use crate::server::Daemon;

    /// PLX-338: a worker start that read its Project before `project/delete` removed it records
    /// no run once the row is gone, so the delete leaves no orphan behind.
    #[tokio::test]
    async fn a_start_racing_a_project_delete_records_no_run() {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::for_tests(dir.path(), 10, Duration::from_secs(90));
        let project = Uuid::now_v7();
        let fields = ProjectFields {
            name: "app".to_owned(),
            repo_path: "/src/app".to_owned(),
            icon: None,
        };
        // The start's `prepare_run` saw the project; the delete then removed it.
        store(&daemon, move |db| {
            db.create_project(project, &fields)
                .map_err(|e| store_error(&e))?;
            db.delete_project(project).map_err(|e| store_error(&e))
        })
        .await
        .unwrap();

        let run_id = RunId::generate();
        let run = RunFields {
            project_id: project,
            prompt: "Build it.".to_owned(),
            requested_account: None,
            policy: super::WORKSPACE_WRITE.to_owned(),
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
        };
        let state = RunState::default();
        let error = record(
            &daemon,
            run_id,
            (run, state),
            None,
            Path::new("/src/app"),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.parallax_data().map(|data| data.kind),
            Some(ErrorKind::ProjectNotFound)
        );
        let stored = store(&daemon, move |db| {
            db.get_run(run_id.into()).map_err(|e| store_error(&e))
        })
        .await
        .unwrap();
        assert_eq!(stored, None);
    }

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
        let retry_task = tokio::spawn({
            let retry = Arc::clone(&retry);
            async move {
                let _guard = retry.lock_owned().await;
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!retry_task.is_finished(), "the retry is still queued");

        // Only the map, the first attempt's guard, and the queued retry may hold the lock when
        // `release` runs, as in real use (RYA-91): the test's own `first`/`retry` clones would
        // keep the entry alive by themselves and hide a `release` that removes it too eagerly.
        // Checked with a `Weak`, which also serves the sweep check at the end.
        let old = Arc::downgrade(&retry);
        drop((first, retry));

        // The first attempt "fails" and releases, exactly as `agents::start`/`actor_for` do on any
        // error path. `Starting::drop` calls `release` while its guard is still alive, so the
        // guard is dropped only after the check below (RYA-91): dropping it first let the retry
        // take the lock on the other worker before the check ran.
        locks.release(id);

        // A caller arriving after the release, while the retry is still queued, must still be
        // handed the SAME lock: nothing has succeeded yet, so there is no fast path (`agents.
        // actor(id)`/`existing()`) to protect a third caller from racing the retry.
        let fresh = locks.get(id);
        assert!(
            Arc::ptr_eq(&old.upgrade().unwrap(), &fresh),
            "a caller after the release still contends for the queued retry's own lock"
        );
        drop(first_guard);

        retry_task.await.unwrap();

        // Once nobody but the map itself holds it, the *next* `get` sweeps it away and a later
        // caller gets a brand-new, uncontended lock: the entry doesn't leak forever. Checked with
        // the `Weak` rather than comparing the new `Arc`'s address to the old one's: once the old
        // allocation is freed, a new one is free to reuse the very same address, which would make
        // a raw-pointer comparison an unreliable false negative.
        drop(fresh);
        let after = locks.get(id);
        assert!(
            old.upgrade().is_none(),
            "the swept lock is still kept alive somewhere"
        );
        drop(after);
    }
}
