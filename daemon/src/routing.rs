//! Routes a task to a backend and account (#119), for M3's runner (#156) to start.
//!
//! [`resolve`] picks the account: the one a task names, or its role's stored default
//! (`accounts/defaults/get` and `accounts/defaults/set`, `methods::defaults`), and forces the
//! coordinator's no-write policy regardless of what was asked for (0004). [`start`] then starts
//! the run, and retries once on a key-account fallback if it fails signed out or rate limited
//! (0004's fallback).
//!
//! What checks a coordinator's turn for a policy violation lives here too, in [`snapshot`] and
//! [`check`]: a backend's own arguments and tool list already keep a no-write run from calling a
//! write tool (0004), but this is 0004's second check, and it doesn't depend on any one backend.
//!
//! # What owns calling this, and how
//!
//! Nothing in wispd calls `resolve`, `start`, `snapshot`, or `check` yet; this module only
//! produces the values their eventual caller needs (see #119's decision record, 0012):
//!
//! - #156 (the M3 runner, workers only) calls `resolve` and `start` for a worker's
//!   `workspace-write` run, maps [`Event::AccountFallback`] to an `agent/*` notification, and
//!   charges usage after it to `to_account`, not the account the run started on.
//! - Whichever M4 issue runs a coordinator's turn (0012, since M4's task issues don't exist yet)
//!   calls `resolve` and `start` the same way, and additionally calls [`snapshot`] before the
//!   turn and [`check`] after it, stopping the run and reporting a `policyViolation` event on a
//!   violation.
//! - [`start`]'s returned [`Started::run`] already forwards to whichever attempt is actually
//!   running, including after a fallback (see [`FallbackRun`]), so a caller never needs to track
//!   that itself.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use sha2::{Digest, Sha256};
use wisp_protocol::{AccountChoice, AccountId, Provider, Role};

use crate::backend::key_account::{self, KeyAccountError};
use crate::backend::{
    AccountRef, Backend, Credential, EVENT_BUFFER, Event, EventSink, EventStream, Failure,
    FailureKind, FollowUp, ModelUsage, Outcome, Run, RunId, RunRequest, SendError, StartError,
    Started, ToolPolicy,
};
use crate::keystore::KeyStore;

/// Every backend wispd can route to, by the provider whose credentials it takes (0004: a backend
/// takes both a subscription login and a key account for the same provider).
#[derive(Clone, Default)]
pub struct BackendRegistry {
    by_provider: HashMap<Provider, Arc<dyn Backend>>,
}

impl BackendRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `backend` as the one that takes `provider`'s credentials.
    pub fn register(&mut self, provider: Provider, backend: Arc<dyn Backend>) -> &mut Self {
        self.by_provider.insert(provider, backend);
        self
    }

    /// The backend registered for `provider`.
    #[must_use]
    pub fn by_provider(&self, provider: Provider) -> Option<Arc<dyn Backend>> {
        self.by_provider.get(&provider).cloned()
    }

    /// The provider and backend whose [`Backend::name`] is `name`, such as `claude`.
    #[must_use]
    pub fn by_backend_name(&self, name: &str) -> Option<(Provider, Arc<dyn Backend>)> {
        self.by_provider
            .iter()
            .find(|(_, backend)| backend.name() == name)
            .map(|(provider, backend)| (*provider, Arc::clone(backend)))
    }
}

/// What routing needs to know about key accounts (#117) besides their keys, which
/// [`key_account::resolve`] reads from the Keychain only once a route is about to start.
pub trait KeyAccounts: Send + Sync {
    /// `id`'s provider, or `None` if no key account has that id.
    fn provider_of(&self, id: AccountId) -> Option<Provider>;

    /// A key account configured for `provider`, to fall back to when a subscription run for it
    /// fails, or `None` if none exists. M2 has no separate "fallback account" setting (#119): any
    /// key account for the same provider serves.
    fn fallback_for(&self, provider: Provider) -> Option<AccountId>;
}

/// Both roles' default accounts, as `accounts/defaults/get` reports them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Defaults {
    /// The coordinator's default account.
    pub coordinator: Option<AccountChoice>,
    /// A worker's default account.
    pub worker: Option<AccountChoice>,
}

impl Defaults {
    fn for_role(&self, role: Role) -> Option<&AccountChoice> {
        match role {
            Role::Coordinator => self.coordinator.as_ref(),
            Role::Worker => self.worker.as_ref(),
        }
    }
}

/// Which account [`resolve`] chose, before its credential is read.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Selection {
    Subscription { backend: String },
    Key { id: AccountId },
}

impl Selection {
    fn into_account_ref(
        self,
        keys: &dyn KeyStore,
        provider: Provider,
    ) -> Result<AccountRef, KeyAccountError> {
        match self {
            Self::Subscription { backend } => Ok(AccountRef {
                id: backend,
                credential: Credential::Subscription { config_home: None },
            }),
            Self::Key { id } => {
                let credential = key_account::resolve(keys, provider, id)?;
                Ok(AccountRef {
                    id: id.to_string(),
                    credential,
                })
            }
        }
    }
}

/// The policy `role` actually gets: always [`ToolPolicy::NoWrite`] for [`Role::Coordinator`],
/// whatever `policy` asks for (0004).
fn enforced_policy(role: Role, policy: ToolPolicy) -> ToolPolicy {
    if role == Role::Coordinator {
        ToolPolicy::NoWrite
    } else {
        policy
    }
}

/// What [`resolve`] found for a task: its backend, the account it will run as (once its
/// credential is read), and the policy it actually gets, which may not be what was requested
/// (0004's coordinator policy).
///
/// Every field is private. A coordinator's forced [`ToolPolicy::NoWrite`] is meaningless if
/// something between `resolve` and `start` can change it back, so nothing outside this module can
/// read or write one without going through [`Resolved::policy`], and [`start`] enforces it again
/// from [`Resolved::role`] regardless of what `policy()` already says.
pub struct Resolved {
    backend: Arc<dyn Backend>,
    provider: Provider,
    selection: Selection,
    role: Role,
    policy: ToolPolicy,
}

impl Resolved {
    /// The account this run will use, once its credential is read (#118): a backend's name for a
    /// subscription, or a key account's id.
    #[must_use]
    pub fn account_id(&self) -> String {
        match &self.selection {
            Selection::Subscription { backend } => backend.clone(),
            Selection::Key { id } => id.to_string(),
        }
    }

    /// The role this run is for.
    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }

    /// The policy this run actually gets.
    #[must_use]
    pub fn policy(&self) -> ToolPolicy {
        self.policy
    }
}

impl std::fmt::Debug for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolved")
            .field("backend", &self.backend.name())
            .field("provider", &self.provider)
            .field("selection", &self.selection)
            .field("role", &self.role)
            .field("policy", &self.policy)
            .finish()
    }
}

/// Why [`resolve`] could not route a task.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RoutingError {
    /// Nothing was requested, and `role` has no default set.
    #[error("{role:?} has no account: none was requested and no default is set")]
    NoAccount {
        /// The role that has no account.
        role: Role,
    },
    /// No backend is registered for a requested `AccountChoice::Subscription`'s backend name.
    #[error("no backend named {backend:?} is registered")]
    UnknownBackend {
        /// The backend name that was requested.
        backend: String,
    },
    /// No backend is registered for a key account's provider.
    #[error("no backend is registered for {provider:?}")]
    UnknownProvider {
        /// The provider that has no backend.
        provider: Provider,
    },
    /// A requested `AccountChoice::Key` names an id [`KeyAccounts::provider_of`] doesn't know.
    #[error("no key account {id} exists")]
    UnknownKeyAccount {
        /// The id that was requested.
        id: AccountId,
    },
    /// The request named a kind of account this build does not know.
    #[error("this build does not know how to route that kind of account")]
    UnknownChoice,
}

/// Picks `role`'s account and backend: `requested` if given, else `role`'s entry in `defaults`.
/// The coordinator always gets [`ToolPolicy::NoWrite`], whatever `policy` asks for (0004); a
/// worker gets `policy` as given.
///
/// This reads no credential and starts nothing; [`start`] does both, right before spawning the
/// backend's process, so a key sits in memory for as little time as possible (#118).
///
/// # Errors
///
/// [`RoutingError`] if nothing was requested and `role` has no default, or if the chosen account
/// names a backend or key account this host doesn't have.
pub fn resolve(
    backends: &BackendRegistry,
    accounts: &dyn KeyAccounts,
    defaults: &Defaults,
    role: Role,
    requested: Option<AccountChoice>,
    policy: ToolPolicy,
) -> Result<Resolved, RoutingError> {
    let choice = requested
        .or_else(|| defaults.for_role(role).cloned())
        .ok_or(RoutingError::NoAccount { role })?;
    let (provider, backend, selection) = match choice {
        AccountChoice::Subscription { backend: name } => {
            let (provider, backend) =
                backends
                    .by_backend_name(&name)
                    .ok_or_else(|| RoutingError::UnknownBackend {
                        backend: name.clone(),
                    })?;
            (provider, backend, Selection::Subscription { backend: name })
        }
        AccountChoice::Key { id } => {
            let provider = accounts
                .provider_of(id)
                .ok_or(RoutingError::UnknownKeyAccount { id })?;
            let backend = backends
                .by_provider(provider)
                .ok_or(RoutingError::UnknownProvider { provider })?;
            (provider, backend, Selection::Key { id })
        }
        AccountChoice::Unknown => return Err(RoutingError::UnknownChoice),
    };
    Ok(Resolved {
        backend,
        provider,
        selection,
        role,
        policy: enforced_policy(role, policy),
    })
}

/// Starts `request` through `resolved`, reading its credential first.
///
/// If the run fails as [`FailureKind::NotSignedIn`] or [`FailureKind::RateLimited`] and
/// `resolved`'s account is a subscription with a key account configured for the same provider
/// (`accounts.fallback_for`), starts once more on that key account and reports the switch as an
/// [`Event::AccountFallback`], before the fallback run's own events, which are charged to
/// `to_account` (see the events for the accounting story). Never retries twice, and never retries
/// a key account's own failure (0004: fallback only ever goes from a subscription to a key
/// account).
///
/// # Errors
///
/// [`StartError`] if the credential can't be read, or the first attempt can't be started. A
/// fallback attempt that can't be started, or whose credential can't be read, is not an error
/// here: the original failure is reported instead, since it is still a real answer.
pub fn start(
    keys: Arc<dyn KeyStore>,
    accounts: &dyn KeyAccounts,
    resolved: Resolved,
    mut request: RunRequest,
) -> Result<Started, StartError> {
    let Resolved {
        backend,
        provider,
        selection,
        role,
        policy,
    } = resolved;
    // Re-enforced here, not just trusted from `resolved`: `Resolved`'s fields are private and
    // `policy` is already correct by construction, but the coordinator's no-write policy is
    // exactly the thing that must never depend on one code path remembering to apply it.
    let policy = enforced_policy(role, policy);
    let is_subscription = matches!(selection, Selection::Subscription { .. });
    let account = selection
        .into_account_ref(keys.as_ref(), provider)
        .map_err(|error| StartError::Invalid(error.to_string()))?;
    request.account = account.clone();
    request.policy = policy;
    let started = backend.start(request.clone())?;

    let fallback_id = is_subscription
        .then(|| accounts.fallback_for(provider))
        .flatten();
    let Some(fallback_id) = fallback_id else {
        return Ok(started);
    };

    let (sink, events) = EventSink::channel(EVENT_BUFFER, usage_baseline(&request));
    let run = Arc::new(FallbackRun::new(Arc::clone(&started.run)));
    let plan = FallbackPlan {
        keys,
        backend,
        provider,
        account_id: fallback_id,
        run: Arc::clone(&run),
    };
    tokio::spawn(drive_with_fallback(
        sink,
        request,
        account.id,
        started.events,
        plan,
    ));
    Ok(Started { run, events })
}

fn usage_baseline(request: &RunRequest) -> Vec<ModelUsage> {
    request
        .resume
        .as_ref()
        .map(|resume| resume.usage_totals.clone())
        .unwrap_or_default()
}

/// A [`Run`] handle that can be pointed at a different run's handle after it was built.
///
/// [`start`] returns one of these instead of the first attempt's own handle whenever a fallback
/// is possible: [`drive_with_fallback`] swaps in the fallback's handle the moment it starts, so
/// `cancel` and `send` always reach whichever process is actually running, including after the
/// switch. Without this, a caller holding the handle `start` returned would cancel a process that
/// already exited while the account actually running, and billing, kept going.
struct FallbackRun {
    current: Mutex<Arc<dyn Run>>,
    /// Whether [`Run::cancel`] has been called. A [`swap`](Self::swap) after that cancels its new
    /// target at once, the same way [`crate::backend::CancelSwitch::arm`] cancels a process armed
    /// after the cancel already happened.
    cancelled: AtomicBool,
}

impl FallbackRun {
    fn new(initial: Arc<dyn Run>) -> Self {
        Self {
            current: Mutex::new(initial),
            cancelled: AtomicBool::new(false),
        }
    }

    fn current(&self) -> Arc<dyn Run> {
        Arc::clone(&self.current.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn swap(&self, next: &Arc<dyn Run>) {
        *self.current.lock().unwrap_or_else(PoisonError::into_inner) = Arc::clone(next);
        if self.cancelled.load(Ordering::Acquire) {
            next.cancel();
        }
    }
}

impl Run for FallbackRun {
    fn id(&self) -> RunId {
        self.current().id()
    }

    fn send(&self, message: FollowUp) -> Result<(), SendError> {
        self.current().send(message)
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.current().cancel();
    }
}

/// What [`drive_with_fallback`] needs to start the fallback attempt, once it decides to.
struct FallbackPlan {
    keys: Arc<dyn KeyStore>,
    backend: Arc<dyn Backend>,
    provider: Provider,
    account_id: AccountId,
    run: Arc<FallbackRun>,
}

fn finished_failed(failure: Failure) -> Event {
    Event::Finished {
        outcome: Outcome::Failed(failure),
        usage_totals: Vec::new(),
    }
}

/// Forwards `current`'s events to `sink`. If it ends in a fallback-eligible failure, points
/// `plan.run` at the fallback's handle, resets `sink`'s running usage total to the fallback
/// request's own baseline (so `Finished.usage_totals` reports only its session, not this
/// account's), and forwards that run's events instead, with an [`Event::AccountFallback`] first.
/// See [`start`].
async fn drive_with_fallback(
    mut sink: EventSink,
    mut request: RunRequest,
    from_account: String,
    mut current: EventStream,
    plan: FallbackPlan,
) {
    let FallbackPlan {
        keys,
        backend,
        provider,
        account_id: fallback_id,
        run,
    } = plan;
    let failure = loop {
        let Some(event) = current.next().await else {
            return;
        };
        if let Event::Finished {
            outcome: Outcome::Failed(failure),
            ..
        } = &event
        {
            break failure.clone();
        }
        if sink.emit(event).await.is_err() {
            return;
        }
    };

    if !matches!(
        failure.failure,
        FailureKind::NotSignedIn | FailureKind::RateLimited
    ) {
        let _ = sink.emit(finished_failed(failure)).await;
        return;
    }
    let Ok(credential) = key_account::resolve(keys.as_ref(), provider, fallback_id) else {
        let _ = sink.emit(finished_failed(failure)).await;
        return;
    };
    let to_account = fallback_id.to_string();
    request.account = AccountRef {
        id: to_account.clone(),
        credential,
    };
    let Ok(started) = backend.start(request.clone()) else {
        let _ = sink.emit(finished_failed(failure)).await;
        return;
    };
    run.swap(&started.run);
    sink.reset_usage(usage_baseline(&request));
    if sink
        .emit(Event::AccountFallback {
            from_account,
            to_account,
            reason: failure.failure,
        })
        .await
        .is_err()
    {
        return;
    }
    let mut fallback_events = started.events;
    while let Some(event) = fallback_events.next().await {
        if sink.emit(event).await.is_err() {
            return;
        }
    }
}

/// A fingerprint of a working tree's tracked and untracked state, taken by [`snapshot`] before a
/// coordinator's turn and compared by [`check`] after it. Equal snapshots mean nothing changed,
/// whether or not the tree was already dirty when the turn started (0004: "if `git status`
/// changes during its turn").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeSnapshot([u8; 32]);

/// Why [`snapshot`] or [`check`] could not read the working tree.
#[derive(Debug, thiserror::Error)]
pub enum PolicyCheckError {
    /// `git` could not be run at all.
    #[error("could not run git: {0}")]
    Spawn(#[from] std::io::Error),
    /// The blocking task that ran `git` panicked.
    #[error("checking the working tree panicked: {0}")]
    Panicked(String),
    /// `git` itself failed, such as when `repo_path` is not a git repository.
    #[error("git failed: {stderr}")]
    GitFailed {
        /// Its stderr.
        stderr: String,
    },
}

/// Takes a fingerprint of `repo_path`'s working tree: `git status --porcelain=v1 -z
/// --untracked-files=all --ignore-submodules=none`, a `git diff --binary --no-ext-diff` against
/// `HEAD` (or the empty tree, in a repository with no commits yet), and the contents of every
/// untracked file the status lists, all under one hash. [`check`] compares it against a later
/// snapshot to tell whether a coordinator's no-write turn changed anything (0004): a tree that was
/// already dirty when this is taken and stays exactly as dirty is not a violation.
///
/// Runs git with `-c core.fsmonitor=false`, so an untrusted repo's `fsmonitor` hook never runs as
/// part of wispd, `GIT_OPTIONAL_LOCKS=0`, so this never waits on or takes the user's index lock,
/// and `--no-ext-diff`, so a repo's configured `diff.external` never runs inside wispd either.
///
/// This only ever sees what `git status` and `git diff` see: a write to a file `.gitignore`
/// excludes passes uncaught (0004 accepts this; see decision 0012).
///
/// # Errors
///
/// [`PolicyCheckError`] if `git` could not be run or failed, such as when `repo_path` is not a
/// git repository.
pub async fn snapshot(repo_path: &Path) -> Result<TreeSnapshot, PolicyCheckError> {
    let repo_path = repo_path.to_owned();
    let digest = tokio::task::spawn_blocking(move || fingerprint(&repo_path))
        .await
        .map_err(|error| PolicyCheckError::Panicked(error.to_string()))??;
    Ok(TreeSnapshot(digest))
}

/// 0004's second check on the coordinator's no-write policy: compares a fresh [`snapshot`] of
/// `repo_path` against `before`, which the caller took earlier, such as right before the turn
/// started. Returns the [`Failure`] to end the run with if anything changed, or `None` if the
/// tree matches `before`, dirty or not.
///
/// # Errors
///
/// [`PolicyCheckError`], the same as [`snapshot`].
pub async fn check(
    repo_path: &Path,
    before: &TreeSnapshot,
) -> Result<Option<Failure>, PolicyCheckError> {
    let after = snapshot(repo_path).await?;
    if after == *before {
        return Ok(None);
    }
    // Cheap next to the hashing `snapshot` already did, and only run on the rare violation path:
    // a second, human-readable status naming what changed, for 0004's "shows the diff" (#119's
    // review, N5). If this second call itself fails, the violation is still reported, just
    // without the paths.
    let paths = changed_paths(repo_path).await.unwrap_or_default();
    let message = if paths.is_empty() {
        "the coordinator's no-write turn changed the working tree".to_owned()
    } else {
        format!(
            "the coordinator's no-write turn changed the working tree:\n{}",
            paths.join("\n")
        )
    };
    Ok(Some(Failure {
        failure: FailureKind::PolicyViolation,
        message,
        exit: None,
        stderr_tail: None,
    }))
}

/// The empty tree's well-known object id, the same for every git repository: `git hash-object -t
/// tree /dev/null`. [`fingerprint`] diffs against it instead of `HEAD` in a repository with no
/// commits yet, where `HEAD` doesn't resolve to anything `git diff` can use.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// What [`fingerprint`] diffs the working tree against: `HEAD` once it resolves to a commit, or
/// the empty tree before the repository's first commit, so a coordinator working in a brand new
/// project doesn't fail every turn's check.
fn diff_target(repo_path: &Path) -> Result<&'static str, PolicyCheckError> {
    let resolves = std::process::Command::new("git")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .current_dir(repo_path)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?
        .success();
    Ok(if resolves { "HEAD" } else { EMPTY_TREE })
}

fn fingerprint(repo_path: &Path) -> Result<[u8; 32], PolicyCheckError> {
    let status = run_git(
        repo_path,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    )?;
    let target = diff_target(repo_path)?;
    let diff = run_git(repo_path, &["diff", target, "--binary", "--no-ext-diff"])?;
    let mut hasher = Sha256::new();
    hasher.update(&status);
    hasher.update(&diff);
    for path in untracked_paths(&status) {
        hasher.update(&path);
        hasher.update([0]);
        if let Ok(contents) = std::fs::read(repo_path.join(OsStr::from_bytes(&path))) {
            hasher.update(&contents);
        }
    }
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&hasher.finalize());
    Ok(digest)
}

/// The paths `git status` lists as changed, one per line, for a violation's message. A separate,
/// human-readable call from [`fingerprint`]'s hashed one, made only once a violation is already
/// known.
async fn changed_paths(repo_path: &Path) -> Result<Vec<String>, PolicyCheckError> {
    let repo_path = repo_path.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        run_git(
            &repo_path,
            &[
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
                "--ignore-submodules=none",
            ],
        )
    })
    .await
    .map_err(|error| PolicyCheckError::Panicked(error.to_string()))??;
    Ok(String::from_utf8_lossy(&output)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect())
}

fn run_git(repo_path: &Path, args: &[&str]) -> Result<Vec<u8>, PolicyCheckError> {
    let output = std::process::Command::new("git")
        .arg("-c")
        .arg("core.fsmonitor=false")
        .args(args)
        .current_dir(repo_path)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .output()?;
    if !output.status.success() {
        return Err(PolicyCheckError::GitFailed {
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(output.stdout)
}

/// The paths of `??` (untracked) entries in `-z`-terminated porcelain output.
fn untracked_paths(porcelain_z: &[u8]) -> Vec<Vec<u8>> {
    porcelain_z
        .split(|&b| b == 0)
        .filter(|entry| entry.starts_with(b"?? "))
        .map(|entry| entry[3..].to_vec())
        .collect()
}

#[cfg(test)]
mod tests;
