//! Normal threads (#110, decision 0017): agents with no coordinator, behind the `threads`
//! capability.
//!
//! A thread is an agent run that belongs to a repo entry instead of a project: its run's
//! `project_id`, and so its `agent.*` events and `agent/list`, use the entry's id. The runner
//! ([`crate::agents`]) starts, streams, messages, cancels, commits, and resumes it exactly as it
//! does a worker. This module adds the repo entries, the scratch repositories that threads with
//! no repo run in, archiving, and deleting.
//!
//! Each thread with no repo gets its own scratch repository at `<data folder>/scratch/<run id>`,
//! so one quick chat can't read another's work. They all belong to wispd's scratch entry, whose
//! path is the `scratch` folder, made on first use.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use jiff::Timestamp;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    ErrorKind, ProjectId, Repo, RepoAddParams, RepoAddResult, RepoId, RunId, Thread,
    ThreadArchiveParams, ThreadArchiveResult, ThreadDeleteResult, ThreadListResult,
    ThreadStartParams, ThreadStartResult, WispEvent,
};
use wisp_store::RepoFields;

use crate::agents::{self, NewRun, NewThread};
use crate::repo;
use crate::server::Daemon;

/// The folder under wispd's data folder that holds threads' scratch repositories.
const SCRATCH_DIR: &str = "scratch";

/// The scratch entry's name.
const SCRATCH_NAME: &str = "No Repo";

const MAX_PATH_BYTES: usize = 1024;

async fn store<T: Send + 'static>(
    daemon: &Daemon,
    job: impl FnOnce(&mut wisp_store::Store) -> Result<T, ErrorObject> + Send + 'static,
) -> Result<T, ErrorObject> {
    daemon.store.run(&CancellationToken::new(), job).await
}

fn store_error(error: &wisp_store::StoreError) -> ErrorObject {
    agents::store_error(error)
}

fn corrupt(what: &str, id: Uuid) -> ErrorObject {
    ErrorObject::internal_error(format!("the stored {what} {id} has an invalid id"))
}

/// A store row as the protocol's repo entry.
pub(crate) fn repo_entry(row: wisp_store::Repo) -> Result<Repo, ErrorObject> {
    Ok(Repo {
        id: RepoId::try_from(row.id).map_err(|_| corrupt("repo entry", row.id))?,
        name: row.fields.name,
        path: row.fields.path,
        scratch: row.fields.scratch,
        created_at: row.created_at,
    })
}

/// A store row as the protocol's thread.
pub(crate) fn thread_entry(row: &wisp_store::Thread) -> Result<Thread, ErrorObject> {
    Ok(Thread {
        id: RunId::try_from(row.id).map_err(|_| corrupt("thread", row.id))?,
        repo: RepoId::try_from(row.repo_id).map_err(|_| corrupt("thread", row.id))?,
        archived: row.archived,
        created_at: row.created_at,
    })
}

fn thread_not_found(id: RunId) -> ErrorObject {
    ErrorObject::wisp(
        ErrorKind::ThreadNotFound,
        format!("no thread has run id {id}"),
    )
}

/// The repository a run's scope stands for: a project's, or a repo entry's path. For the scratch
/// entry it is the folder that holds the scratch repositories.
pub(crate) fn scope_path(db: &wisp_store::Store, scope: ProjectId) -> Result<String, ErrorObject> {
    if let Some(project) = db.get_project(scope.into()).map_err(|e| store_error(&e))? {
        return Ok(project.repo_path);
    }
    if let Some(repo) = db.get_repo(scope.into()).map_err(|e| store_error(&e))? {
        return Ok(repo.fields.path);
    }
    Err(ErrorObject::wisp(
        ErrorKind::ProjectNotFound,
        format!("no project has id {scope}"),
    ))
}

/// The thread row of run `id`, for a retried `thread/start`. A run that isn't a thread's has
/// its id taken.
pub(crate) async fn existing_thread(
    daemon: &Arc<Daemon>,
    id: RunId,
) -> Result<wisp_store::Thread, ErrorObject> {
    store(daemon, move |db| {
        db.get_thread(id.into())
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| {
                ErrorObject::wisp(
                    ErrorKind::IdConflict,
                    format!("run {id} exists and is not a thread"),
                )
            })
    })
    .await
}

/// Reports a new thread as `thread.started`, a host-level event.
pub(crate) fn log_started(daemon: &Daemon, row: &wisp_store::Thread) {
    match thread_entry(row) {
        Ok(thread) => {
            daemon
                .log
                .append(thread.created_at, None, WispEvent::ThreadStarted { thread });
        }
        Err(error) => warn!(error = %error.message, "could not report a new thread"),
    }
}

/// `thread/list`.
pub(crate) async fn list(daemon: &Arc<Daemon>) -> Result<ThreadListResult, ErrorObject> {
    let log = Arc::clone(&daemon.log);
    store(daemon, move |db| {
        let repos = db.list_repos().map_err(|e| store_error(&e))?;
        let threads = db.list_threads().map_err(|e| store_error(&e))?;
        let seq = log.head();
        Ok(ThreadListResult {
            repos: repos
                .into_iter()
                .map(repo_entry)
                .collect::<Result<_, _>>()?,
            threads: threads.iter().map(thread_entry).collect::<Result<_, _>>()?,
            seq,
        })
    })
    .await
}

fn check_path(path: &str) -> Result<(), ErrorObject> {
    if path.len() > MAX_PATH_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "path must be at most {MAX_PATH_BYTES} bytes"
        )));
    }
    if path.contains('\0') {
        return Err(ErrorObject::invalid_params("path must not contain NUL"));
    }
    if !Path::new(path).is_absolute() {
        return Err(ErrorObject::invalid_params("path must be an absolute path"));
    }
    let has_dot_segment = Path::new(path)
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        || path.contains("/./")
        || path.ends_with("/.");
    if has_dot_segment {
        return Err(ErrorObject::invalid_params(
            "path must not contain . or .. segments",
        ));
    }
    Ok(())
}

/// `repo/add`: registers the repository at `path`, or returns the entry it already has.
pub(crate) async fn add_repo(
    daemon: &Arc<Daemon>,
    params: RepoAddParams,
) -> Result<RepoAddResult, ErrorObject> {
    let RepoAddParams { id, path } = params;
    check_path(&path)?;
    let not_a_repository = |message: String| ErrorObject::wisp(ErrorKind::NotARepository, message);
    repo::check(Path::new(&path)).map_err(|error| not_a_repository(error.to_string()))?;
    let canonical = Path::new(&path)
        .canonicalize()
        .map_err(|error| not_a_repository(format!("{path} can't be resolved: {error}")))?;
    let data_dir = daemon
        .data_dir
        .root()
        .canonicalize()
        .unwrap_or_else(|_| daemon.data_dir.root().to_owned());
    let owned = ["worktrees", SCRATCH_DIR]
        .iter()
        .any(|folder| canonical.starts_with(data_dir.join(folder)));
    if owned {
        return Err(not_a_repository(format!(
            "{path} is one of wisp's own worktrees or scratch repositories"
        )));
    }
    let Some(canonical) = canonical.to_str().map(str::to_owned) else {
        return Err(not_a_repository(format!("{path} is not valid UTF-8")));
    };
    let name = Path::new(&canonical)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&canonical)
        .to_owned();
    let log = Arc::clone(&daemon.log);
    store(daemon, move |db| {
        let fields = RepoFields {
            name,
            path: canonical,
            scratch: false,
        };
        let (repo, created) = add(db, id.into(), &fields)?;
        let repo = repo_entry(repo)?;
        if created {
            log.append(
                repo.created_at,
                None,
                WispEvent::RepoAdded { repo: repo.clone() },
            );
            info!(repo = %repo.id, path = %repo.path, "registered a repository for threads");
        }
        Ok(RepoAddResult { repo })
    })
    .await
}

/// Adds a repo entry, and says whether it is new.
fn add(
    db: &mut wisp_store::Store,
    id: Uuid,
    fields: &RepoFields,
) -> Result<(wisp_store::Repo, bool), ErrorObject> {
    let known = db
        .list_repos()
        .map_err(|e| store_error(&e))?
        .into_iter()
        .any(|repo| repo.fields.path == fields.path);
    let repo = db.add_repo(id, fields).map_err(|error| match error {
        wisp_store::StoreError::IdConflict { id } => ErrorObject::wisp(
            ErrorKind::IdConflict,
            format!("repo entry {id} exists with another path"),
        ),
        other => store_error(&other),
    })?;
    Ok((repo, !known))
}

/// The folder that holds threads' scratch repositories, created and canonical.
fn scratch_root(daemon: &Daemon) -> Result<PathBuf, ErrorObject> {
    let root = daemon.data_dir.root().join(SCRATCH_DIR);
    std::fs::create_dir_all(&root)
        .and_then(|()| root.canonicalize())
        .map_err(|error| {
            ErrorObject::internal_error(format!(
                "could not create the scratch folder {}: {error}",
                root.display()
            ))
        })
}

/// wispd's scratch entry, made on first use.
async fn scratch_entry(daemon: &Arc<Daemon>) -> Result<wisp_store::Repo, ErrorObject> {
    let root = scratch_root(daemon)?;
    let log = Arc::clone(&daemon.log);
    store(daemon, move |db| {
        if let Some(repo) = db.scratch_repo().map_err(|e| store_error(&e))? {
            return Ok(repo);
        }
        let fields = RepoFields {
            name: SCRATCH_NAME.to_owned(),
            path: root.to_string_lossy().into_owned(),
            scratch: true,
        };
        let (row, created) = add(db, RepoId::generate().into(), &fields)?;
        if created {
            let repo = repo_entry(row.clone())?;
            log.append(repo.created_at, None, WispEvent::RepoAdded { repo });
        }
        Ok(row)
    })
    .await
}

/// `thread/start`: see the module documentation. Idempotent on the run id.
pub(crate) async fn start(
    daemon: Arc<Daemon>,
    params: ThreadStartParams,
) -> Result<ThreadStartResult, ErrorObject> {
    let ThreadStartParams {
        run_id,
        repo,
        prompt,
        account,
    } = params;
    let entry = match repo {
        Some(id) => {
            store(&daemon, move |db| {
                db.get_repo(id.into())
                    .map_err(|e| store_error(&e))?
                    .ok_or_else(|| {
                        ErrorObject::wisp(
                            ErrorKind::RepoNotFound,
                            format!("no repo entry has id {id}"),
                        )
                    })
            })
            .await?
        }
        None => scratch_entry(&daemon).await?,
    };
    let scope = ProjectId::try_from(entry.id).map_err(|_| corrupt("repo entry", entry.id))?;
    let scratch = if entry.fields.scratch {
        let dir = PathBuf::from(&entry.fields.path).join(run_id.to_string());
        daemon
            .agents
            .worktrees()
            .init_scratch(&dir)
            .await
            .map_err(|error| {
                ErrorObject::wisp(
                    ErrorKind::WorktreeFailed,
                    format!("could not make the thread's scratch repository: {error}"),
                )
            })?;
        Some(dir)
    } else {
        None
    };
    let new = NewRun {
        run_id,
        scope,
        prompt,
        account,
        thread: Some(NewThread {
            scratch: scratch.clone(),
        }),
    };
    let created = match agents::create(Arc::clone(&daemon), new).await {
        Ok(created) => created,
        Err(error) => {
            if let Some(dir) = scratch {
                remove_unused_scratch(&daemon, run_id, &dir).await;
            }
            return Err(error);
        }
    };
    let thread = created
        .thread
        .as_ref()
        .ok_or_else(|| ErrorObject::internal_error("a new thread has no thread row"))?;
    Ok(ThreadStartResult {
        thread: thread_entry(thread)?,
        run: created.run,
    })
}

/// Removes a scratch repository made for a `thread/start` that didn't create its run.
async fn remove_unused_scratch(daemon: &Arc<Daemon>, run_id: RunId, dir: &Path) {
    let exists = store(daemon, move |db| {
        db.get_run(run_id.into())
            .map(|row| row.is_some())
            .map_err(|e| store_error(&e))
    })
    .await;
    if matches!(exists, Ok(false)) {
        remove_scratch(daemon, run_id, dir);
    }
}

/// Removes `dir`, a thread's scratch repository, after checking that it is exactly
/// `<scratch folder>/<run id>`, so a stored path can never send the removal elsewhere.
fn remove_scratch(daemon: &Daemon, run_id: RunId, dir: &Path) {
    let Ok(root) = scratch_root(daemon) else {
        return;
    };
    let expected = root.join(run_id.to_string());
    let is_expected = dir
        .canonicalize()
        .is_ok_and(|canonical| canonical == expected);
    let is_folder = std::fs::symlink_metadata(&expected).is_ok_and(|meta| meta.is_dir());
    if !is_expected || !is_folder {
        warn!(run = %run_id, dir = %dir.display(), "not removing a scratch repository outside the scratch folder");
        return;
    }
    if let Err(error) = std::fs::remove_dir_all(&expected) {
        warn!(run = %run_id, %error, "could not remove a thread's scratch repository");
    }
}

/// `thread/archive`.
pub(crate) async fn archive(
    daemon: &Arc<Daemon>,
    params: ThreadArchiveParams,
) -> Result<ThreadArchiveResult, ErrorObject> {
    let ThreadArchiveParams { run_id, archived } = params;
    let log = Arc::clone(&daemon.log);
    store(daemon, move |db| {
        let before = db
            .get_thread(run_id.into())
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| thread_not_found(run_id))?;
        if before.archived == archived {
            return Ok(ThreadArchiveResult {
                thread: thread_entry(&before)?,
            });
        }
        let row = db
            .set_thread_archived(run_id.into(), archived)
            .map_err(|error| match error {
                wisp_store::StoreError::NotFound { .. } => thread_not_found(run_id),
                other => store_error(&other),
            })?;
        let thread = thread_entry(&row)?;
        log.append(
            Timestamp::now(),
            None,
            WispEvent::ThreadUpdated {
                thread: thread.clone(),
            },
        );
        Ok(ThreadArchiveResult { thread })
    })
    .await
}

/// `thread/delete`: refused while the run's CLI runs. Removes the rows first, then the worktree,
/// its branch, and a scratch repository; startup's garbage collection removes a worktree folder
/// that a crash left behind.
pub(crate) async fn delete(
    daemon: &Arc<Daemon>,
    run_id: RunId,
) -> Result<ThreadDeleteResult, ErrorObject> {
    let agents = &daemon.agents;
    let _creating = agents.creation_lock().await;
    let (thread, worktree) = store(daemon, move |db| {
        let thread = db
            .get_thread(run_id.into())
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| thread_not_found(run_id))?;
        if let Some(run) = db.get_run(run_id.into()).map_err(|e| store_error(&e))?
            && agents::is_active(&run)
        {
            return Err(ErrorObject::wisp(
                ErrorKind::RunActive,
                "the thread's agent is still running; stop it before deleting the thread",
            ));
        }
        let worktree = db
            .get_worktree(run_id.into())
            .map_err(|e| store_error(&e))?;
        Ok((thread, worktree))
    })
    .await?;
    agents.forget(run_id);
    store(daemon, move |db| {
        db.delete_thread(run_id.into()).map_err(|e| store_error(&e))
    })
    .await?;
    if let Some(worktree) = worktree {
        let repo_path = PathBuf::from(&worktree.repo_path);
        if let Err(error) = agents
            .worktrees()
            .remove(&repo_path, Path::new(&worktree.path), &worktree.branch)
            .await
        {
            warn!(run = %run_id, %error, "could not remove a deleted thread's worktree");
        }
        let scratch = store(daemon, move |db| {
            db.get_repo(thread.repo_id)
                .map(|repo| repo.is_some_and(|repo| repo.fields.scratch))
                .map_err(|e| store_error(&e))
        })
        .await
        .unwrap_or(false);
        if scratch {
            remove_scratch(daemon, run_id, &repo_path);
        }
    }
    let repo = RepoId::try_from(thread.repo_id).map_err(|_| corrupt("thread", thread.id))?;
    daemon.log.append(
        Timestamp::now(),
        None,
        WispEvent::ThreadDeleted { run_id, repo },
    );
    info!(run = %run_id, "deleted a thread");
    Ok(ThreadDeleteResult {})
}

#[cfg(test)]
mod tests {
    use wisp_protocol::jsonrpc::INVALID_PARAMS;

    use super::check_path;

    #[test]
    fn repo_paths_are_absolute_and_plain() {
        assert!(check_path("/Users/me/src/wisp").is_ok());
        for path in ["src/wisp", "/Users/me/../wisp", "/Users/me/./wisp", "/a\0b"] {
            assert_eq!(check_path(path).unwrap_err().code, INVALID_PARAMS, "{path}");
        }
        let long = format!("/{}", "p".repeat(super::MAX_PATH_BYTES));
        assert_eq!(check_path(&long).unwrap_err().code, INVALID_PARAMS);
    }
}
