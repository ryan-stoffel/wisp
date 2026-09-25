use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wisp_protocol::{AccountChoice, AccountId, Provider, Role};

use super::{
    BackendRegistry, Defaults, KeyAccounts, PolicyCheckError, RoutingError, check, resolve,
    snapshot, start,
};
use crate::backend::{
    AccountRef, Backend, CancelSwitch, Capabilities, Credential, EVENT_BUFFER, Event, EventSink,
    EventStream, Failure, FailureKind, ModelUsage, Outcome, RunHandle, RunId, RunRequest,
    StartError, Started, ToolPolicy, Usage,
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

#[test]
fn an_explicit_choice_is_used_even_when_the_roles_default_differs() {
    let backend: Arc<dyn Backend> = Arc::new(ScriptedBackend::new(Vec::new()));
    let registry = registry(backend);
    let key_id = AccountId::generate();
    let mut accounts = FixedAccounts::default();
    accounts.providers.insert(key_id, Provider::Anthropic);
    // The default is a subscription; the request below explicitly asks for the key account
    // instead, which must win.
    let defaults = Defaults {
        coordinator: None,
        worker: Some(subscription_choice()),
    };

    let resolved = resolve(
        &registry,
        &accounts,
        &defaults,
        Role::Worker,
        Some(AccountChoice::Key { id: key_id }),
        ToolPolicy::WorkspaceWrite,
    )
    .unwrap();
    assert_eq!(resolved.account_id(), key_id.to_string());
}

#[test]
fn the_roles_default_is_used_when_nothing_is_requested() {
    let backend: Arc<dyn Backend> = Arc::new(ScriptedBackend::new(Vec::new()));
    let registry = registry(backend);
    let accounts = FixedAccounts::default();
    let defaults = Defaults {
        coordinator: None,
        worker: Some(subscription_choice()),
    };

    let resolved = resolve(
        &registry,
        &accounts,
        &defaults,
        Role::Worker,
        None,
        ToolPolicy::WorkspaceWrite,
    )
    .unwrap();
    assert_eq!(resolved.account_id(), "claude");
}

#[test]
fn the_coordinator_always_gets_no_write_however_it_is_requested() {
    let backend: Arc<dyn Backend> = Arc::new(ScriptedBackend::new(Vec::new()));
    let registry = registry(backend);
    let accounts = FixedAccounts::default();
    let defaults = Defaults::default();

    let resolved = resolve(
        &registry,
        &accounts,
        &defaults,
        Role::Coordinator,
        Some(subscription_choice()),
        ToolPolicy::WorkspaceWrite,
    )
    .unwrap();
    assert_eq!(resolved.policy(), ToolPolicy::NoWrite);
}

#[test]
fn a_worker_keeps_the_policy_it_was_given() {
    let backend: Arc<dyn Backend> = Arc::new(ScriptedBackend::new(Vec::new()));
    let registry = registry(backend);
    let accounts = FixedAccounts::default();
    let defaults = Defaults::default();

    let resolved = resolve(
        &registry,
        &accounts,
        &defaults,
        Role::Worker,
        Some(subscription_choice()),
        ToolPolicy::WorkspaceWrite,
    )
    .unwrap();
    assert_eq!(resolved.policy(), ToolPolicy::WorkspaceWrite);
}

#[test]
fn no_default_and_nothing_requested_is_an_error() {
    let backend: Arc<dyn Backend> = Arc::new(ScriptedBackend::new(Vec::new()));
    let registry = registry(backend);
    let accounts = FixedAccounts::default();
    let defaults = Defaults::default();

    let error = resolve(
        &registry,
        &accounts,
        &defaults,
        Role::Worker,
        None,
        ToolPolicy::WorkspaceWrite,
    )
    .unwrap_err();
    assert_eq!(error, RoutingError::NoAccount { role: Role::Worker });
}

#[test]
fn an_unregistered_backend_name_is_an_error() {
    let backend: Arc<dyn Backend> = Arc::new(ScriptedBackend::new(Vec::new()));
    let registry = registry(backend);
    let accounts = FixedAccounts::default();
    let defaults = Defaults::default();

    let error = resolve(
        &registry,
        &accounts,
        &defaults,
        Role::Worker,
        Some(AccountChoice::Subscription {
            backend: "codex".into(),
        }),
        ToolPolicy::WorkspaceWrite,
    )
    .unwrap_err();
    assert_eq!(
        error,
        RoutingError::UnknownBackend {
            backend: "codex".into()
        }
    );
}

#[test]
fn an_unknown_key_account_id_is_an_error() {
    let backend: Arc<dyn Backend> = Arc::new(ScriptedBackend::new(Vec::new()));
    let registry = registry(backend);
    let accounts = FixedAccounts::default();
    let defaults = Defaults::default();
    let id = AccountId::generate();

    let error = resolve(
        &registry,
        &accounts,
        &defaults,
        Role::Worker,
        Some(AccountChoice::Key { id }),
        ToolPolicy::WorkspaceWrite,
    )
    .unwrap_err();
    assert_eq!(error, RoutingError::UnknownKeyAccount { id });
}

#[test]
fn a_key_accounts_provider_with_no_backend_is_an_error() {
    let backend: Arc<dyn Backend> = Arc::new(ScriptedBackend::new(Vec::new()));
    let registry = registry(backend);
    let mut accounts = FixedAccounts::default();
    let id = AccountId::generate();
    accounts.providers.insert(id, Provider::Openai);
    let defaults = Defaults::default();

    let error = resolve(
        &registry,
        &accounts,
        &defaults,
        Role::Worker,
        Some(AccountChoice::Key { id }),
        ToolPolicy::WorkspaceWrite,
    )
    .unwrap_err();
    assert_eq!(
        error,
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

fn resolved_subscription(backend: Arc<dyn Backend>) -> super::Resolved {
    resolved_for_role(backend, Role::Worker, ToolPolicy::WorkspaceWrite)
}

fn resolved_for_role(backend: Arc<dyn Backend>, role: Role, policy: ToolPolicy) -> super::Resolved {
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

    assert_eq!(backend.calls()[0].policy, ToolPolicy::NoWrite);
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
async fn a_rate_limited_subscription_also_falls_back() {
    let fallback_id = AccountId::generate();
    let backend = Arc::new(ScriptedBackend::new(vec![
        vec![finished(Outcome::Failed(failure(FailureKind::RateLimited)))],
        vec![finished(Outcome::Completed { result: None })],
    ]));
    let resolved = resolved_subscription(backend.clone());
    let accounts = accounts_with_fallback(fallback_id);
    let keys = key_store_with(fallback_id, "sk-ant-fallback-key");

    let mut started = start(keys, &accounts, resolved, request(&root())).unwrap();
    let events = rest(&mut started.events).await;
    assert_eq!(
        events[0],
        Event::AccountFallback {
            from_account: "claude".into(),
            to_account: fallback_id.to_string(),
            reason: FailureKind::RateLimited,
        }
    );
    assert_eq!(backend.calls().len(), 2);
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
    // A fallback is "configured", but starting from a key account must never use it: 0004 allows
    // only subscription-to-key fallback, never the reverse.
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

// ---------------------------------------------------------------------------------------------
// snapshot() and check()
// ---------------------------------------------------------------------------------------------

fn git(repo: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .status()
        .expect("git must be on PATH to run this test");
    assert!(status.success(), "git {args:?} failed");
}

fn committed_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q"]);
    git(
        dir.path(),
        &["config", "user.email", "wisp-test@example.com"],
    );
    git(dir.path(), &["config", "user.name", "wisp tests"]);
    std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "-q", "-m", "initial"]);
    dir
}

#[tokio::test]
async fn a_clean_working_tree_has_no_violation() {
    let dir = committed_repo();
    let before = snapshot(dir.path()).await.unwrap();
    assert_eq!(check(dir.path(), &before).await.unwrap(), None);
}

#[tokio::test]
async fn a_change_to_a_tracked_file_is_a_violation() {
    let dir = committed_repo();
    let before = snapshot(dir.path()).await.unwrap();
    std::fs::write(dir.path().join("README.md"), "changed\n").unwrap();

    let violation = check(dir.path(), &before).await.unwrap().unwrap();
    assert_eq!(violation.failure, FailureKind::PolicyViolation);
}

#[tokio::test]
async fn a_new_untracked_file_is_also_a_violation() {
    let dir = committed_repo();
    let before = snapshot(dir.path()).await.unwrap();
    std::fs::write(dir.path().join("new-file.txt"), "surprise").unwrap();

    let violation = check(dir.path(), &before).await.unwrap().unwrap();
    assert_eq!(violation.failure, FailureKind::PolicyViolation);
}

#[tokio::test]
async fn a_tree_that_was_already_dirty_and_stays_that_way_has_no_violation() {
    let dir = committed_repo();
    std::fs::write(dir.path().join("README.md"), "dirty before the turn\n").unwrap();
    std::fs::write(dir.path().join("already-there.txt"), "also dirty before").unwrap();

    let before = snapshot(dir.path()).await.unwrap();
    assert_eq!(
        check(dir.path(), &before).await.unwrap(),
        None,
        "a tree that started dirty and stayed exactly that way is not a violation"
    );
}

#[tokio::test]
async fn a_further_edit_to_an_already_modified_file_is_still_a_violation() {
    let dir = committed_repo();
    std::fs::write(dir.path().join("README.md"), "first change\n").unwrap();
    let before = snapshot(dir.path()).await.unwrap();

    std::fs::write(
        dir.path().join("README.md"),
        "second change, during the turn\n",
    )
    .unwrap();

    let violation = check(dir.path(), &before).await.unwrap().unwrap();
    assert_eq!(violation.failure, FailureKind::PolicyViolation);
}

#[tokio::test]
async fn a_directory_that_is_not_a_git_repository_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let error = snapshot(dir.path()).await.unwrap_err();
    assert!(
        matches!(error, PolicyCheckError::GitFailed { .. }),
        "{error:?}"
    );
}
