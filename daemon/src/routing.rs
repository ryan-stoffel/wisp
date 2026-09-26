//! Routes a task to a backend and account for the runner (`crate::agents`) to start.
//!
//! [`resolve`] picks the account: the one a task names, or its role's stored default. [`start`]
//! forces the coordinator's no-write policy regardless of what was asked for, starts the run, and
//! retries once on a key-account fallback if it fails signed out or rate limited. Its returned
//! [`Started::run`] forwards to whichever attempt is actually running (see [`FallbackRun`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use wisp_protocol::{AccountChoice, AccountId, Provider, Role};

use crate::backend::key_account::{self, KeyAccountError};
use crate::backend::{
    AccountRef, Backend, Credential, EVENT_BUFFER, Event, EventSink, EventStream, Failure,
    FailureKind, FollowUp, ModelUsage, Outcome, Run, RunId, RunRequest, SendError, StartError,
    Started, ToolPolicy,
};
use crate::keystore::KeyStore;

/// Every backend wispd can route to, by the provider whose credentials it takes: a backend takes
/// both a subscription login and a key account for the same provider.
#[derive(Clone, Default)]
pub struct BackendRegistry {
    by_provider: HashMap<Provider, Arc<dyn Backend>>,
}

impl std::fmt::Debug for BackendRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map()
            .entries(
                self.by_provider
                    .iter()
                    .map(|(provider, backend)| (provider, backend.name())),
            )
            .finish()
    }
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

/// What routing needs to know about key accounts besides their keys, which
/// [`key_account::resolve`] reads from the Keychain only once a route is about to start.
pub trait KeyAccounts: Send + Sync {
    /// `id`'s provider, or `None` if no key account has that id.
    fn provider_of(&self, id: AccountId) -> Option<Provider>;

    /// A key account configured for `provider`, to fall back to when a subscription run for it
    /// fails, or `None` if none exists. Any key account for the same provider serves.
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
/// credential is read), its role, and the policy it asked for.
///
/// Every field is private, so nothing between `resolve` and `start` can change the role that
/// [`start`] enforces the coordinator's no-write policy from.
pub struct Resolved {
    backend: Arc<dyn Backend>,
    provider: Provider,
    selection: Selection,
    role: Role,
    policy: ToolPolicy,
}

impl Resolved {
    /// The account this run will use, once its credential is read: a backend's name for a
    /// subscription, or a key account's id.
    #[must_use]
    pub fn account_id(&self) -> String {
        match &self.selection {
            Selection::Subscription { backend } => backend.clone(),
            Selection::Key { id } => id.to_string(),
        }
    }

    /// The backend that will run it, to check what it can do before starting.
    #[must_use]
    pub fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
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
///
/// This reads no credential and starts nothing; [`start`] does both, right before spawning the
/// backend's process, so a key sits in memory for as little time as possible.
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
        policy,
    })
}

/// Starts `request` through `resolved`, reading its credential first. The coordinator always
/// gets [`ToolPolicy::NoWrite`], whatever `resolved` asked for; a worker gets its policy as given.
///
/// If the run fails as [`FailureKind::NotSignedIn`] or [`FailureKind::RateLimited`] and
/// `resolved`'s account is a subscription with a key account configured for the same provider
/// (`accounts.fallback_for`), starts once more on that key account and reports the switch as an
/// [`Event::AccountFallback`], before the fallback run's own events, which are charged to
/// `to_account`. Never retries twice, and never retries a key account's own failure: fallback
/// only ever goes from a subscription to a key account.
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
    let policy = if role == Role::Coordinator {
        ToolPolicy::NoWrite
    } else {
        policy
    };
    let is_subscription = matches!(selection, Selection::Subscription { .. });
    let account = selection
        .into_account_ref(keys.as_ref(), provider)
        .map_err(|error| StartError::Invalid(error.to_string()))?;
    request.account = account.clone();
    request.policy = policy;
    if policy == ToolPolicy::NoWrite {
        // A no-write run never writes, so it has no worker sandbox, whatever was asked.
        request.sandbox = None;
    }
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

#[cfg(test)]
mod tests;
