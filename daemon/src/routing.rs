//! Routes a task to a backend and account (#119), for M3's runner (#156) to start.
//!
//! [`resolve`] picks the account: the one a task names, or its role's stored default
//! (`accounts/defaults/get` and `accounts/defaults/set`, `methods::defaults`), and forces the
//! coordinator's no-write policy regardless of what was asked for (0004). [`start`] then starts
//! the run, and retries once on a key-account fallback if it fails signed out or rate limited
//! (0004's fallback).
//!
//! What checks a coordinator's turn for a policy violation lives here too, in
//! [`check_no_write_policy`]: a backend's own arguments and tool list already keep a no-write run
//! from calling a write tool (0004), but this is 0004's second check, and it doesn't depend on any
//! one backend.
//!
//! # What M3 still has to do
//!
//! [`start`]'s returned [`Started::run`] is always the first attempt's handle. A fallback swaps
//! which process is actually running, but sending or cancelling through the old handle no longer
//! reaches it. M3's runner already has to keep a run's control handle by `runId` and can re-point
//! it at the new one when it sees [`Event::AccountFallback`]; this module doesn't try to do that
//! itself.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use wisp_protocol::{AccountChoice, AccountId, Provider, Role};

use crate::backend::key_account::{self, KeyAccountError};
use crate::backend::{
    AccountRef, Backend, Credential, EVENT_BUFFER, Event, EventSink, EventStream, Failure,
    FailureKind, Outcome, RunRequest, StartError, Started, ToolPolicy,
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

/// What [`resolve`] found for a task: its backend, the account it will run as (once its
/// credential is read), and the policy it actually gets, which may not be what was requested
/// (0004's coordinator policy).
pub struct Resolved {
    backend: Arc<dyn Backend>,
    provider: Provider,
    selection: Selection,
    /// The policy this run actually gets.
    pub policy: ToolPolicy,
}

impl std::fmt::Debug for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolved")
            .field("backend", &self.backend.name())
            .field("provider", &self.provider)
            .field("selection", &self.selection)
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
    let policy = if role == Role::Coordinator {
        ToolPolicy::NoWrite
    } else {
        policy
    };
    Ok(Resolved {
        backend,
        provider,
        selection,
        policy,
    })
}

/// Starts `request` through `resolved`, reading its credential first.
///
/// If the run fails as [`FailureKind::NotSignedIn`] or [`FailureKind::RateLimited`] and
/// `resolved`'s account is a subscription with a key account configured for the same provider
/// (`accounts.fallback_for`), starts once more on that key account and reports the switch as an
/// [`Event::AccountFallback`], before the fallback run's own events. Never retries twice, and
/// never retries a key account's own failure (0004: fallback only ever goes from a subscription to
/// a key account).
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
        policy,
    } = resolved;
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

    let baseline = request
        .resume
        .as_ref()
        .map(|resume| resume.usage_totals.clone())
        .unwrap_or_default();
    let (sink, events) = EventSink::channel(EVENT_BUFFER, baseline);
    let run = Arc::clone(&started.run);
    let plan = FallbackPlan {
        keys,
        backend,
        provider,
        account_id: fallback_id,
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

/// What [`drive_with_fallback`] needs to start the fallback attempt, once it decides to.
struct FallbackPlan {
    keys: Arc<dyn KeyStore>,
    backend: Arc<dyn Backend>,
    provider: Provider,
    account_id: AccountId,
}

/// Forwards `current`'s events to `sink`. If it ends in a fallback-eligible failure, starts once
/// more on `plan`'s key account and forwards that run's events instead, with an
/// [`Event::AccountFallback`] first. See [`start`].
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
        let _ = sink
            .emit(Event::Finished {
                outcome: Outcome::Failed(failure),
                usage_totals: Vec::new(),
            })
            .await;
        return;
    }
    let Ok(credential) = key_account::resolve(keys.as_ref(), provider, fallback_id) else {
        let _ = sink
            .emit(Event::Finished {
                outcome: Outcome::Failed(failure),
                usage_totals: Vec::new(),
            })
            .await;
        return;
    };
    request.account = AccountRef {
        id: fallback_id.to_string(),
        credential,
    };
    let Ok(started) = backend.start(request) else {
        let _ = sink
            .emit(Event::Finished {
                outcome: Outcome::Failed(failure),
                usage_totals: Vec::new(),
            })
            .await;
        return;
    };
    if sink
        .emit(Event::AccountFallback {
            from_account,
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

/// Why [`check_no_write_policy`] could not check the working tree.
#[derive(Debug, thiserror::Error)]
pub enum PolicyCheckError {
    /// `git` could not be run at all.
    #[error("could not run git: {0}")]
    Spawn(#[from] std::io::Error),
    /// The blocking task that ran `git` panicked.
    #[error("checking the working tree panicked: {0}")]
    Panicked(String),
    /// `git status` itself failed, such as when `repo_path` is not a git repository.
    #[error("git status failed: {stderr}")]
    GitFailed {
        /// Its stderr.
        stderr: String,
    },
}

/// 0004's second check on the coordinator's no-write policy: after each of its turns, wispd runs
/// `git status --porcelain` in the project's repository. Anything in its output, tracked or not,
/// means something wrote to the tree despite the policy.
///
/// Returns the [`Failure`] to end the run with, or `None` if the tree is clean.
///
/// # Errors
///
/// [`PolicyCheckError`] if `git` could not be run or failed, such as when `repo_path` is not a
/// git repository.
pub async fn check_no_write_policy(repo_path: &Path) -> Result<Option<Failure>, PolicyCheckError> {
    let repo_path = repo_path.to_owned();
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&repo_path)
            .output()
    })
    .await
    .map_err(|error| PolicyCheckError::Panicked(error.to_string()))??;
    if !output.status.success() {
        return Err(PolicyCheckError::GitFailed {
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    if output.stdout.is_empty() {
        return Ok(None);
    }
    Ok(Some(Failure {
        failure: FailureKind::PolicyViolation,
        message: format!(
            "the coordinator's no-write turn changed the working tree:\n{}",
            String::from_utf8_lossy(&output.stdout).trim_end()
        ),
        exit: None,
        stderr_tail: None,
    }))
}

#[cfg(test)]
mod tests;
