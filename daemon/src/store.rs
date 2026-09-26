//! The project store, on a thread of its own, and the mapping between its rows and the
//! protocol's types.
//!
//! SQLite calls block, so one thread owns [`wisp_store::Store`] and runs the jobs that requests
//! send it, one at a time. Running them in order is also what keeps a snapshot consistent:
//! `project/create` appends its event in the job that writes the row, and `project/list` reads the
//! head `seq` in the job that reads the rows.
//!
//! A job whose request is cancelled is skipped if it hasn't started. Once it has started, it runs
//! to the end and the request gets its real result, so -32800 always means nothing was done.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};
use uuid::Uuid;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AccountChoice, AccountId, ErrorKind, KeyAccount, Project, ProjectCreateParams, ProjectId,
    Provider, Role, StoreState,
};
use wisp_store::{AccountFields, ProjectFields, RoleDefault, Store, StoreError};

use crate::repo;

const QUEUED: u8 = 0;
const STARTED: u8 = 1;
const CANCELLED: u8 = 2;

type Job = Box<dyn FnOnce(&mut Store) + Send>;

pub(crate) enum Message {
    Job(Job),
    Stop,
}

pub(crate) enum StoreHandle {
    Open {
        jobs: mpsc::Sender<Message>,
        thread: Mutex<Option<JoinHandle<()>>>,
    },
    Unavailable,
}

impl StoreHandle {
    /// Opens the store at `path` and starts its thread. If it can't be opened, wispd keeps
    /// running without it: `host/health` says so, and project methods fail.
    pub fn open(path: &Path) -> Self {
        let store = match Store::open(path) {
            Ok(store) => store,
            Err(error) => {
                error!(path = %path.display(), %error, "could not open the project store");
                return Self::Unavailable;
            }
        };
        let (jobs, queue) = mpsc::channel();
        let spawned = thread::Builder::new()
            .name("wispd-store".to_owned())
            .spawn(move || run(store, &queue));
        match spawned {
            Ok(thread) => {
                info!(path = %path.display(), "opened the project store");
                Self::Open {
                    jobs,
                    thread: Mutex::new(Some(thread)),
                }
            }
            Err(error) => {
                error!(%error, "could not start the project store's thread");
                Self::Unavailable
            }
        }
    }

    pub fn state(&self) -> StoreState {
        let Self::Open { thread, .. } = self else {
            return StoreState::Unavailable;
        };
        let running = thread
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .is_some_and(|thread| !thread.is_finished());
        if running {
            StoreState::Ok
        } else {
            StoreState::Unavailable
        }
    }

    /// Runs `job` on the store's thread and returns its result, or an error for a request that
    /// was cancelled before the job started, or a store that is unavailable.
    pub async fn run<T: Send + 'static>(
        &self,
        cancel: &CancellationToken,
        job: impl FnOnce(&mut Store) -> Result<T, ErrorObject> + Send + 'static,
    ) -> Result<T, ErrorObject> {
        let Self::Open { jobs, .. } = self else {
            return Err(unavailable());
        };
        let status = Arc::new(AtomicU8::new(QUEUED));
        let (reply, mut result) = oneshot::channel();
        let job_status = Arc::clone(&status);
        let job_cancel = cancel.clone();
        // The job checks the token itself too, so it is skipped even when the request's task
        // was aborted and nobody is waiting for it.
        let job: Job = Box::new(move |store| {
            if job_cancel.is_cancelled() {
                job_status.store(CANCELLED, Ordering::Release);
                return;
            }
            if job_status
                .compare_exchange(QUEUED, STARTED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                let _ = reply.send(job(store));
            }
        });
        jobs.send(Message::Job(job)).map_err(|_| unavailable())?;
        let finished = tokio::select! {
            biased;
            finished = &mut result => finished,
            () = cancel.cancelled() => {
                match status.compare_exchange(QUEUED, CANCELLED, Ordering::AcqRel, Ordering::Acquire) {
                    Ok(_) | Err(CANCELLED) => return Err(ErrorObject::request_cancelled()),
                    Err(_) => (&mut result).await,
                }
            }
        };
        finished.unwrap_or_else(|_| {
            if status.load(Ordering::Acquire) == CANCELLED {
                Err(ErrorObject::request_cancelled())
            } else {
                Err(unavailable())
            }
        })
    }

    /// Stops the thread after the job it is running, and closes the database.
    pub async fn stop(&self) {
        let Self::Open { jobs, thread } = self else {
            return;
        };
        let _ = jobs.send(Message::Stop);
        let thread = thread.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(thread) = thread {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

fn run(mut store: Store, queue: &mpsc::Receiver<Message>) {
    while let Ok(Message::Job(job)) = queue.recv() {
        // A panicking job drops its reply, which fails only its own request.
        if catch_unwind(AssertUnwindSafe(|| job(&mut store))).is_err() {
            error!("a project store job panicked");
        }
    }
}

fn unavailable() -> ErrorObject {
    ErrorObject::internal_error("the project store is unavailable")
}

/// The protocol error for a store error.
pub(crate) fn store_error(error: &StoreError) -> ErrorObject {
    match error {
        StoreError::IdConflict { id } => ErrorObject::wisp(
            ErrorKind::IdConflict,
            format!("project {id} exists with a different name or repository"),
        ),
        StoreError::NotFound { id } => ErrorObject::wisp(
            ErrorKind::ProjectNotFound,
            format!("no project has id {id}"),
        ),
        other => failed(other),
    }
}

fn failed(error: &StoreError) -> ErrorObject {
    error!(%error, "the project store failed");
    ErrorObject::internal_error(format!("the project store failed: {error}"))
}

/// The store's id and fields for a `project/create`.
pub(crate) fn fields(params: ProjectCreateParams) -> (Uuid, ProjectFields) {
    (
        params.id.into(),
        ProjectFields {
            name: params.name,
            repo_path: params.repo_path,
        },
    )
}

/// A store row as the protocol's project, with the branch its repository has checked out now.
///
/// wispd writes only version 7 ids, so a row with another kind of id was written by something
/// else, and the request fails rather than hide the row.
pub(crate) fn project(row: wisp_store::Project) -> Result<Project, ErrorObject> {
    let id = ProjectId::try_from(row.id).map_err(|_| {
        error!(id = %row.id, "a stored project's id is not a UUIDv7");
        ErrorObject::internal_error(format!("the stored project {} has an invalid id", row.id))
    })?;
    Ok(Project {
        id,
        name: row.name,
        branch: repo::branch(Path::new(&row.repo_path)),
        repo_path: row.repo_path,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// The protocol error for an accounts-table store error.
///
/// Unlike [`store_error`], a missing row is `accountNotFound`, not `projectNotFound`: the two
/// tables share [`StoreError`], but not its meaning.
pub(crate) fn account_store_error(error: &StoreError) -> ErrorObject {
    match error {
        StoreError::IdConflict { id } => ErrorObject::wisp(
            ErrorKind::IdConflict,
            format!("key account {id} exists with a different provider, label, or key"),
        ),
        StoreError::NotFound { id } => ErrorObject::wisp(
            ErrorKind::AccountNotFound,
            format!("no key account has id {id}"),
        ),
        other => failed(other),
    }
}

/// A stored provider string as the protocol's [`Provider`]. Anything this build does not
/// recognize decodes as [`Provider::Unknown`], the same forward-compatibility rule the protocol
/// itself uses.
fn provider_from_text(text: &str) -> Provider {
    match text {
        "anthropic" => Provider::Anthropic,
        "openai" => Provider::Openai,
        "cursor" => Provider::Cursor,
        _ => Provider::Unknown,
    }
}

/// The store's fields for an `accounts/keys/add`, with `provider` as its stored text.
pub(crate) fn account_fields(
    provider: Provider,
    label: String,
    masked_key: String,
) -> AccountFields {
    let provider = match provider {
        Provider::Anthropic => "anthropic",
        Provider::Openai => "openai",
        Provider::Cursor => "cursor",
        Provider::Unknown => "unknown",
    };
    AccountFields {
        provider: provider.to_owned(),
        label,
        masked_key,
    }
}

/// `role`'s text for the `role_defaults.role` column.
pub(crate) fn role_text(role: Role) -> &'static str {
    match role {
        Role::Coordinator => "coordinator",
        Role::Worker => "worker",
    }
}

/// A stored [`RoleDefault`] as the protocol's [`AccountChoice`].
///
/// wispd writes only version 7 ids, so a row with another kind of id was written by something
/// else, and the request fails rather than hide the row.
pub(crate) fn account_choice(default: RoleDefault) -> Result<AccountChoice, ErrorObject> {
    match default {
        RoleDefault::Subscription { backend } => Ok(AccountChoice::Subscription { backend }),
        RoleDefault::Key { account_id } => {
            let id = AccountId::try_from(account_id).map_err(|_| {
                error!(id = %account_id, "a stored role default's key account id is not a UUIDv7");
                ErrorObject::internal_error(format!(
                    "the stored default account {account_id} has an invalid id"
                ))
            })?;
            Ok(AccountChoice::Key { id })
        }
    }
}

/// Vendor CLIs wispd ships or plans an adapter for, not yet the ones actually installed.
const KNOWN_BACKENDS: &[&str] = &["claude", "codex", "cursor"];

/// An `accounts/defaults/set` choice as the store's [`RoleDefault`], checked against `db_store`
/// first: a `Key` must be a real row in `accounts`, and a `Subscription`'s backend must be one of
/// [`KNOWN_BACKENDS`]. Unvalidated, a typo or a removed key account would only surface later, as a
/// `RoutingError` when a task tries to start.
///
/// # Errors
///
/// `invalidParams`, naming the missing account or backend, or an internal error if the check
/// itself fails.
pub(crate) fn role_default(
    db_store: &wisp_store::Store,
    choice: &AccountChoice,
) -> Result<RoleDefault, ErrorObject> {
    match choice {
        AccountChoice::Subscription { backend } => {
            if KNOWN_BACKENDS.contains(&backend.as_str()) {
                Ok(RoleDefault::Subscription {
                    backend: backend.clone(),
                })
            } else {
                Err(ErrorObject::invalid_params(format!(
                    "{backend:?} is not a backend wispd knows"
                )))
            }
        }
        AccountChoice::Key { id } => {
            let uuid = (*id).into();
            let exists = db_store
                .get_account(uuid)
                .map_err(|error| {
                    error!(error = %error, "the project store failed checking a key account");
                    ErrorObject::internal_error(format!("the project store failed: {error}"))
                })?
                .is_some();
            if exists {
                Ok(RoleDefault::Key { account_id: uuid })
            } else {
                Err(ErrorObject::invalid_params(format!(
                    "no key account {id} exists"
                )))
            }
        }
        AccountChoice::Unknown => Err(ErrorObject::invalid_params(
            "account must be a subscription or a key",
        )),
    }
}

/// A store row as the protocol's key account.
///
/// wispd writes only version 7 ids, so a row with another kind of id was written by something
/// else, and the request fails rather than hide the row.
pub(crate) fn key_account(row: wisp_store::Account) -> Result<KeyAccount, ErrorObject> {
    let id = AccountId::try_from(row.id).map_err(|_| {
        error!(id = %row.id, "a stored key account's id is not a UUIDv7");
        ErrorObject::internal_error(format!(
            "the stored key account {} has an invalid id",
            row.id
        ))
    })?;
    Ok(KeyAccount {
        id,
        provider: provider_from_text(&row.provider),
        label: row.label,
        created_at: row.created_at,
        masked_key: row.masked_key,
    })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;
    use wisp_protocol::jsonrpc::{INTERNAL_ERROR, REQUEST_CANCELLED, WISP_ERROR};
    use wisp_protocol::{AccountId, ErrorKind, ProjectId, Provider, StoreState};
    use wisp_store::StoreError;

    use super::{
        StoreHandle, account_fields, account_store_error, key_account, project, store_error,
    };

    fn row(id: Uuid) -> wisp_store::Project {
        wisp_store::Project {
            id,
            name: "wisp".to_owned(),
            repo_path: "/src/wisp".to_owned(),
            created_at: "2026-09-24T12:00:00.5Z".parse().unwrap(),
            updated_at: "2026-09-24T12:00:01Z".parse().unwrap(),
        }
    }

    #[test]
    fn rows_map_to_protocol_projects_field_for_field() {
        let id = ProjectId::generate();
        let mapped = project(row(id.into())).unwrap();
        assert_eq!(mapped.id, id);
        assert_eq!(mapped.name, "wisp");
        assert_eq!(mapped.repo_path, "/src/wisp");
        assert_eq!(mapped.created_at, row(id.into()).created_at);
        assert_eq!(mapped.updated_at, row(id.into()).updated_at);
    }

    #[test]
    fn a_row_whose_id_is_not_v7_is_an_internal_error() {
        let error = project(row(Uuid::nil())).unwrap_err();
        assert_eq!(error.code, INTERNAL_ERROR);
        let error = key_account(account_row(Uuid::nil())).unwrap_err();
        assert_eq!(error.code, INTERNAL_ERROR);
    }

    #[test]
    fn conflicts_and_missing_projects_are_wisp_errors() {
        let id = Uuid::now_v7();
        let conflict = store_error(&StoreError::IdConflict { id });
        assert_eq!(conflict.code, WISP_ERROR);
        assert_eq!(conflict.wisp_data().unwrap().kind, ErrorKind::IdConflict);
        let missing = store_error(&StoreError::NotFound { id });
        assert_eq!(
            missing.wisp_data().unwrap().kind,
            ErrorKind::ProjectNotFound
        );
        let other = store_error(&StoreError::JournalMode("delete".to_owned()));
        assert_eq!(other.code, INTERNAL_ERROR);
    }

    fn account_row(id: Uuid) -> wisp_store::Account {
        wisp_store::Account {
            id,
            provider: "anthropic".to_owned(),
            label: "Personal".to_owned(),
            masked_key: "sk-ant-...abcd".to_owned(),
            created_at: "2026-09-24T12:00:00.5Z".parse().unwrap(),
        }
    }

    #[test]
    fn account_rows_map_to_protocol_key_accounts_field_for_field() {
        let id = AccountId::generate();
        let mapped = key_account(account_row(id.into())).unwrap();
        assert_eq!(mapped.id, id);
        assert_eq!(mapped.provider, Provider::Anthropic);
        assert_eq!(mapped.label, "Personal");
        assert_eq!(mapped.masked_key, "sk-ant-...abcd");
        assert_eq!(mapped.created_at, account_row(id.into()).created_at);

        let fields = account_fields(
            Provider::Openai,
            "Work".to_owned(),
            "sk-proj-...wxyz".to_owned(),
        );
        assert_eq!(fields.provider, "openai");
        assert_eq!(fields.label, "Work");
        assert_eq!(fields.masked_key, "sk-proj-...wxyz");
    }

    #[test]
    fn an_unrecognized_stored_provider_decodes_as_unknown() {
        let mut row = account_row(Uuid::now_v7());
        row.provider = "gemini".to_owned();
        let mapped = key_account(row).unwrap();
        assert_eq!(mapped.provider, Provider::Unknown);
    }

    #[test]
    fn missing_accounts_are_account_not_found_not_project_not_found() {
        let id = Uuid::now_v7();
        let conflict = account_store_error(&StoreError::IdConflict { id });
        assert_eq!(conflict.wisp_data().unwrap().kind, ErrorKind::IdConflict);
        let missing = account_store_error(&StoreError::NotFound { id });
        assert_eq!(
            missing.wisp_data().unwrap().kind,
            ErrorKind::AccountNotFound
        );
    }

    #[tokio::test]
    async fn a_store_that_cannot_open_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        let store = StoreHandle::open(&file.join("nested.sqlite3"));
        assert_eq!(store.state(), StoreState::Unavailable);
        let error = store
            .run(&CancellationToken::new(), |_| Ok(()))
            .await
            .unwrap_err();
        assert_eq!(error.code, INTERNAL_ERROR);
        assert_eq!(
            error.message,
            "Internal error: the project store is unavailable"
        );
    }

    #[tokio::test]
    async fn a_queued_job_is_skipped_when_cancelled_and_a_started_one_finishes() {
        let dir = tempfile::tempdir().unwrap();
        let store = StoreHandle::open(&dir.path().join("wispd.sqlite3"));
        assert_eq!(store.state(), StoreState::Ok);

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let first_cancel = CancellationToken::new();
        let second_cancel = CancellationToken::new();
        let first = store.run(&first_cancel, move |_| {
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok("first")
        });
        let second = store.run(&second_cancel, |_| Ok("second"));
        let driver = async {
            tokio::task::spawn_blocking(move || started_rx.recv().unwrap())
                .await
                .unwrap();
            first_cancel.cancel();
            second_cancel.cancel();
            tokio::time::sleep(Duration::from_millis(50)).await;
            release_tx.send(()).unwrap();
        };
        let (first, second, ()) = tokio::join!(first, second, driver);
        assert_eq!(first, Ok("first"), "a started job returns its result");
        assert_eq!(second.unwrap_err().code, REQUEST_CANCELLED);
        store.stop().await;
        assert_eq!(store.state(), StoreState::Unavailable);
    }
}
