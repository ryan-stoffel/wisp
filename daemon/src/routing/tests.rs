use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wisp_protocol::{AccountChoice, AccountId, Provider, Role};

use super::{BackendRegistry, Defaults, KeyAccounts, Resolved, RoutingError, resolve, start};
use crate::backend::{
    AccountRef, Backend, CancelSwitch, Capabilities, Credential, EVENT_BUFFER, Event, EventSink,
    EventStream, Failure, FailureKind, ModelUsage, Outcome, RunHandle, RunId, RunRequest,
    StartError, Started, ToolPolicy, Usage, WorkerSandbox, claude,
};
use crate::keystore::{KeyStore, MemoryKeyStore};

fn root() -> PathBuf {
    PathBuf::from("/")
}

fn request(cwd: &Path) -> RunRequest {
    RunRequest {
        run_id: RunId::generate(),
        turn_id: None,
        cwd: cwd.to_owned(),
        prompt: "hi".into(),
        policy: ToolPolicy::WorkspaceWrite,
        sandbox: Some(WorkerSandbox::for_worktree(
            Path::new("/Users/u"),
            Path::new("/Users/u/wisp"),
            cwd,
            Path::new("/Users/u/src/app/.git"),
            Path::new("/Users/u/wisp/context/p"),
        )),
        account: AccountRef {
            id: "unset".into(),
            credential: Credential::Subscription { config_home: None },
        },
        resume: None,
        model: None,
    }
}

fn finished(outcome: Outcome) -> Event {
    Event::Finished {
        outcome,
        usage_totals: Vec::new(),
    }
}

fn failure(kind: FailureKind) -> Failure {
    Failure {
        failure: kind,
        message: format!("{kind:?}"),
        exit: None,
        stderr_tail: None,
    }
}

fn usage(input_tokens: u64) -> Usage {
    Usage {
        input_tokens,
        ..Usage::default()
    }
}

fn usage_event(input_tokens: u64) -> Event {
    Event::Usage(ModelUsage {
        model: None,
        usage: usage(input_tokens),
    })
}

/// A [`Backend`] that emits one canned, pre-built event sequence per call to
/// [`Backend::start`], in order, and records each call's request and the [`CancelSwitch`] behind
/// its handle. Unlike [`crate::backend::fake::FakeBackend`], it spawns no process, so its
/// scripted attempts can differ from one call to the next: exactly what testing a fallback, which
/// restarts the same backend with a different credential, needs.
struct ScriptedBackend {
    attempts: Mutex<VecDeque<Vec<Event>>>,
    calls: Mutex<Vec<RunRequest>>,
    switches: Mutex<Vec<CancelSwitch>>,
}

impl ScriptedBackend {
    fn new(attempts: Vec<Vec<Event>>) -> Self {
        Self {
            attempts: Mutex::new(attempts.into()),
            calls: Mutex::new(Vec::new()),
            switches: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<RunRequest> {
        self.calls.lock().unwrap().clone()
    }

    /// Each call's cancel switch, in order, so a test can tell which attempt a `cancel()` reached.
    fn switches(&self) -> Vec<CancelSwitch> {
        self.switches.lock().unwrap().clone()
    }
}

impl Backend for ScriptedBackend {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        self.calls.lock().unwrap().push(request.clone());
        let events = self
            .attempts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("the scripted backend has no more attempts queued"));
        let (mut sink, stream) = EventSink::channel(EVENT_BUFFER, Vec::new());
        tokio::spawn(async move {
            for event in events {
                if sink.emit(event).await.is_err() {
                    return;
                }
            }
        });
        let switch = CancelSwitch::new();
        self.switches.lock().unwrap().push(switch.clone());
        let (handle, _control) = RunHandle::new(request.run_id, false, switch);
        Ok(Started {
            run: Arc::new(handle),
            events: stream,
        })
    }
}

fn registry(backend: Arc<dyn Backend>) -> BackendRegistry {
    let mut registry = BackendRegistry::new();
    registry.register(Provider::Anthropic, backend);
    registry
}

#[derive(Default)]
struct FixedAccounts {
    providers: HashMap<AccountId, Provider>,
    fallbacks: HashMap<Provider, AccountId>,
}

impl KeyAccounts for FixedAccounts {
    fn provider_of(&self, id: AccountId) -> Option<Provider> {
        self.providers.get(&id).copied()
    }

    fn fallback_for(&self, provider: Provider) -> Option<AccountId> {
        self.fallbacks.get(&provider).copied()
    }
}

async fn next(events: &mut EventStream) -> Event {
    tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .expect("no event within 10s")
        .expect("the stream ended")
}

async fn rest(events: &mut EventStream) -> Vec<Event> {
    let mut all = Vec::new();
    loop {
        let event = next(events).await;
        let terminal = event.is_terminal();
        all.push(event);
        if terminal {
            assert!(events.next().await.is_none(), "Finished must be last");
            return all;
        }
    }
}

fn subscription_choice() -> AccountChoice {
    AccountChoice::Subscription {
        backend: "claude".into(),
    }
}

// ---------------------------------------------------------------------------------------------
// resolve()
// ---------------------------------------------------------------------------------------------

/// Resolves a worker's account against a registry with only Anthropic's backend.
fn resolve_worker(
    accounts: &FixedAccounts,
    defaults: &Defaults,
    requested: Option<AccountChoice>,
) -> Result<Resolved, RoutingError> {
    let backend: Arc<dyn Backend> = Arc::new(ScriptedBackend::new(Vec::new()));
    resolve(
        &registry(backend),
        accounts,
        defaults,
        Role::Worker,
        requested,
        ToolPolicy::WorkspaceWrite,
    )
}

#[test]
fn an_explicit_choice_wins_over_the_roles_default_which_is_used_otherwise() {
    let key_id = AccountId::generate();
    let mut accounts = FixedAccounts::default();
    accounts.providers.insert(key_id, Provider::Anthropic);
    let defaults = Defaults {
        coordinator: None,
        worker: Some(subscription_choice()),
    };

    let explicit = resolve_worker(
        &accounts,
        &defaults,
        Some(AccountChoice::Key { id: key_id }),
    );
    assert_eq!(explicit.unwrap().account_id(), key_id.to_string());
    let default = resolve_worker(&accounts, &defaults, None);
    assert_eq!(default.unwrap().account_id(), "claude");
}

#[test]
fn an_account_this_host_cannot_route_is_an_error() {
    let defaults = Defaults::default();
    let id = AccountId::generate();
    let mut accounts = FixedAccounts::default();
    assert_eq!(
        resolve_worker(&accounts, &defaults, None).unwrap_err(),
        RoutingError::NoAccount { role: Role::Worker }
    );
    let codex = AccountChoice::Subscription {
        backend: "codex".into(),
    };
    assert_eq!(
        resolve_worker(&accounts, &defaults, Some(codex)).unwrap_err(),
        RoutingError::UnknownBackend {
            backend: "codex".into()
        }
    );
    let key = || Some(AccountChoice::Key { id });
    assert_eq!(
        resolve_worker(&accounts, &defaults, key()).unwrap_err(),
        RoutingError::UnknownKeyAccount { id }
    );
    accounts.providers.insert(id, Provider::Openai);
    assert_eq!(
        resolve_worker(&accounts, &defaults, key()).unwrap_err(),
        RoutingError::UnknownProvider {
            provider: Provider::Openai
        }
    );
}

// ---------------------------------------------------------------------------------------------
// start() and its fallback
// ---------------------------------------------------------------------------------------------

fn key_store_with(id: AccountId, key: &str) -> Arc<dyn KeyStore> {
    let store = MemoryKeyStore::new();
    store.set(id, key).unwrap();
    Arc::new(store)
}

fn accounts_with_fallback(id: AccountId) -> FixedAccounts {
    let mut accounts = FixedAccounts::default();
    accounts.providers.insert(id, Provider::Anthropic);
    accounts.fallbacks.insert(Provider::Anthropic, id);
    accounts
}

fn resolved_subscription(backend: Arc<dyn Backend>) -> Resolved {
    resolved_for_role(backend, Role::Worker, ToolPolicy::WorkspaceWrite)
}

fn resolved_for_role(backend: Arc<dyn Backend>, role: Role, policy: ToolPolicy) -> Resolved {
    let registry = registry(backend);
    resolve(
        &registry,
        &FixedAccounts::default(),
        &Defaults::default(),
        role,
        Some(subscription_choice()),
        policy,
    )
    .unwrap()
}

#[tokio::test]
async fn start_sends_no_write_to_the_backend_for_a_coordinator() {
    let backend = Arc::new(ScriptedBackend::new(vec![vec![finished(
        Outcome::Completed { result: None },
    )]]));
    let resolved = resolved_for_role(
        backend.clone(),
        Role::Coordinator,
        ToolPolicy::WorkspaceWrite,
    );
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());

    let mut started = start(keys, &FixedAccounts::default(), resolved, request(&root())).unwrap();
    rest(&mut started.events).await;

    let sent = &backend.calls()[0];
    assert_eq!(sent.policy, ToolPolicy::NoWrite);
    assert_eq!(sent.sandbox, None, "a coordinator gets no worker sandbox");
    let mut expected: Vec<&str> = claude::BASE_ARGS.to_vec();
    expected.extend(claude::NO_WRITE_ARGS);
    assert_eq!(
        claude::arguments(sent).unwrap(),
        expected,
        "Claude runs a coordinator with exactly the no-write flags, none of the sandbox's"
    );
}

#[tokio::test]
async fn start_passes_a_worker_its_sandbox() {
    let backend = Arc::new(ScriptedBackend::new(vec![vec![finished(
        Outcome::Completed { result: None },
    )]]));
    let resolved = resolved_for_role(backend.clone(), Role::Worker, ToolPolicy::WorkspaceWrite);
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());
    let request = request(&root());
    let sandbox = request.sandbox.clone();

    let mut started = start(keys, &FixedAccounts::default(), resolved, request).unwrap();
    rest(&mut started.events).await;

    let sent = &backend.calls()[0];
    assert_eq!(sent.policy, ToolPolicy::WorkspaceWrite);
    assert_eq!(sent.sandbox, sandbox);
    let argv = claude::arguments(sent).unwrap();
    assert!(argv.iter().any(|arg| arg == "--restricted"), "{argv:?}");
}

#[tokio::test]
async fn a_signed_out_subscription_falls_back_once_to_the_configured_key_account() {
    let fallback_id = AccountId::generate();
    let backend = Arc::new(ScriptedBackend::new(vec![
        vec![finished(Outcome::Failed(failure(FailureKind::NotSignedIn)))],
        vec![finished(Outcome::Completed { result: None })],
    ]));
    let resolved = resolved_subscription(backend.clone());
    let accounts = accounts_with_fallback(fallback_id);
    let keys = key_store_with(fallback_id, "sk-ant-fallback-key");

    let mut started = start(keys, &accounts, resolved, request(&root())).unwrap();
    let events = rest(&mut started.events).await;

    assert_eq!(
        events,
        [
            Event::AccountFallback {
                from_account: "claude".into(),
                to_account: fallback_id.to_string(),
                reason: FailureKind::NotSignedIn,
            },
            finished(Outcome::Completed { result: None }),
        ]
    );
    let calls = backend.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(
        calls[0].account.credential,
        Credential::Subscription { config_home: None }
    );
    assert_eq!(calls[1].account.id, fallback_id.to_string());
    let Credential::ApiKey(key) = &calls[1].account.credential else {
        panic!(
            "expected an API key credential, got {:?}",
            calls[1].account.credential
        );
    };
    assert_eq!(key.expose(), "sk-ant-fallback-key");
}

#[tokio::test]
async fn without_a_configured_key_account_there_is_no_fallback() {
    let backend = Arc::new(ScriptedBackend::new(vec![vec![finished(Outcome::Failed(
        failure(FailureKind::NotSignedIn),
    ))]]));
    let resolved = resolved_subscription(backend.clone());
    let accounts = FixedAccounts::default(); // no fallback configured for Anthropic
    let keys: Arc<dyn KeyStore> = Arc::new(MemoryKeyStore::new());

    let mut started = start(keys, &accounts, resolved, request(&root())).unwrap();
    let events = rest(&mut started.events).await;

    assert_eq!(
        events,
        [finished(Outcome::Failed(failure(FailureKind::NotSignedIn)))]
    );
    assert_eq!(backend.calls().len(), 1, "must not have retried");
}

#[tokio::test]
async fn a_key_accounts_own_failure_never_falls_back_to_a_subscription() {
    let some_other_key = AccountId::generate();
    let key_id = AccountId::generate();
    let backend = Arc::new(ScriptedBackend::new(vec![vec![finished(Outcome::Failed(
        failure(FailureKind::NotSignedIn),
    ))]]));
    let registry = registry(backend.clone());
    let mut accounts = FixedAccounts::default();
    accounts.providers.insert(key_id, Provider::Anthropic);
    // A fallback is "configured", but starting from a key account must never use it: fallback
    // only goes from a subscription to a key, never the reverse.
    accounts
        .fallbacks
        .insert(Provider::Anthropic, some_other_key);
    let resolved = resolve(
        &registry,
        &accounts,
        &Defaults::default(),
        Role::Worker,
        Some(AccountChoice::Key { id: key_id }),
        ToolPolicy::WorkspaceWrite,
    )
    .unwrap();
    let keys = key_store_with(key_id, "sk-ant-the-only-key");

    let mut started = start(keys, &accounts, resolved, request(&root())).unwrap();
    let events = rest(&mut started.events).await;

    assert_eq!(
        events,
        [finished(Outcome::Failed(failure(FailureKind::NotSignedIn)))]
    );
    assert_eq!(backend.calls().len(), 1, "must not have retried");
}

#[tokio::test]
async fn a_successful_run_never_falls_back_even_with_a_key_configured() {
    let fallback_id = AccountId::generate();
    let backend = Arc::new(ScriptedBackend::new(vec![vec![finished(
        Outcome::Completed { result: None },
    )]]));
    let resolved = resolved_subscription(backend.clone());
    let accounts = accounts_with_fallback(fallback_id);
    let keys = key_store_with(fallback_id, "sk-ant-fallback-key");

    let mut started = start(keys, &accounts, resolved, request(&root())).unwrap();
    let events = rest(&mut started.events).await;

    assert_eq!(events, [finished(Outcome::Completed { result: None })]);
    assert_eq!(backend.calls().len(), 1);
}

#[tokio::test]
async fn a_failure_that_is_not_signed_out_or_rate_limited_does_not_fall_back() {
    let fallback_id = AccountId::generate();
    let backend = Arc::new(ScriptedBackend::new(vec![vec![finished(Outcome::Failed(
        failure(FailureKind::VendorError),
    ))]]));
    let resolved = resolved_subscription(backend.clone());
    let accounts = accounts_with_fallback(fallback_id);
    let keys = key_store_with(fallback_id, "sk-ant-fallback-key");

    let mut started = start(keys, &accounts, resolved, request(&root())).unwrap();
    let events = rest(&mut started.events).await;

    assert_eq!(
        events,
        [finished(Outcome::Failed(failure(FailureKind::VendorError)))]
    );
    assert_eq!(backend.calls().len(), 1);
}

#[tokio::test]
async fn a_fallback_that_also_fails_is_not_retried_again() {
    let fallback_id = AccountId::generate();
    let backend = Arc::new(ScriptedBackend::new(vec![
        vec![finished(Outcome::Failed(failure(FailureKind::NotSignedIn)))],
        vec![finished(Outcome::Failed(failure(FailureKind::RateLimited)))],
    ]));
    let resolved = resolved_subscription(backend.clone());
    let accounts = accounts_with_fallback(fallback_id);
    let keys = key_store_with(fallback_id, "sk-ant-fallback-key");

    let mut started = start(keys, &accounts, resolved, request(&root())).unwrap();
    let events = rest(&mut started.events).await;

    assert_eq!(
        events,
        [
            Event::AccountFallback {
                from_account: "claude".into(),
                to_account: fallback_id.to_string(),
                reason: FailureKind::NotSignedIn,
            },
            finished(Outcome::Failed(failure(FailureKind::RateLimited))),
        ]
    );
    assert_eq!(backend.calls().len(), 2, "no second fallback attempt");
}

#[tokio::test]
async fn usage_after_a_fallback_is_isolated_to_the_new_account() {
    let fallback_id = AccountId::generate();
    let backend = Arc::new(ScriptedBackend::new(vec![
        vec![
            usage_event(9),
            finished(Outcome::Failed(failure(FailureKind::RateLimited))),
        ],
        vec![
            usage_event(4),
            finished(Outcome::Completed { result: None }),
        ],
    ]));
    let resolved = resolved_subscription(backend.clone());
    let accounts = accounts_with_fallback(fallback_id);
    let keys = key_store_with(fallback_id, "sk-ant-fallback-key");

    let mut started = start(keys, &accounts, resolved, request(&root())).unwrap();
    let events = rest(&mut started.events).await;

    assert_eq!(
        events,
        [
            usage_event(9),
            Event::AccountFallback {
                from_account: "claude".into(),
                to_account: fallback_id.to_string(),
                reason: FailureKind::RateLimited,
            },
            usage_event(4),
            Event::Finished {
                outcome: Outcome::Completed { result: None },
                usage_totals: vec![ModelUsage {
                    model: None,
                    usage: usage(4)
                }],
            },
        ],
        "Finished must total only the fallback account's own usage, not both attempts'"
    );
}

#[tokio::test]
async fn cancel_after_a_fallback_reaches_the_second_attempt() {
    let fallback_id = AccountId::generate();
    let backend = Arc::new(ScriptedBackend::new(vec![
        vec![finished(Outcome::Failed(failure(FailureKind::NotSignedIn)))],
        vec![finished(Outcome::Completed { result: None })],
    ]));
    let resolved = resolved_subscription(backend.clone());
    let accounts = accounts_with_fallback(fallback_id);
    let keys = key_store_with(fallback_id, "sk-ant-fallback-key");

    let mut started = start(keys, &accounts, resolved, request(&root())).unwrap();
    // The handle is swapped to the fallback's before `AccountFallback` is emitted, so seeing this
    // event guarantees the swap has already happened.
    assert_eq!(
        next(&mut started.events).await,
        Event::AccountFallback {
            from_account: "claude".into(),
            to_account: fallback_id.to_string(),
            reason: FailureKind::NotSignedIn,
        }
    );

    started.run.cancel();

    let switches = backend.switches();
    assert_eq!(switches.len(), 2);
    assert!(
        !switches[0].is_cancelled(),
        "the abandoned first attempt must be left alone"
    );
    assert!(
        switches[1].is_cancelled(),
        "cancel must reach the attempt that is actually running"
    );
}
