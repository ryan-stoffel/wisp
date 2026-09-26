//! `context/list`, `context/read`, and `context/write` (0005, #155).
//!
//! File I/O runs on tokio's blocking pool (`spawn_blocking`), not the request's own task, since
//! reading, writing, and listing a project's shared context folder are ordinary blocking
//! filesystem calls, just like `wisp_store`'s calls are blocking SQLite ones.

use std::sync::Arc;

use tracing::info;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    ContextListParams, ContextListResult, ContextReadParams, ContextReadResult, ContextWriteParams,
    ContextWriteResult, ErrorKind, ProjectId, WispEvent,
};
use wisp_store::StoreError;

use super::Context;
use crate::context::{self, Existing};
use crate::store::store_error;

pub(crate) async fn list(
    context: &Context,
    params: ContextListParams,
) -> Result<ContextListResult, ErrorObject> {
    let project = params.project;
    ensure_project_exists(context, project).await?;
    let daemon = Arc::clone(&context.daemon);
    run_blocking(move || {
        let dir = context::ensure_dir(&daemon.data_dir, project).map_err(io_error(""))?;
        let files = context::list_files(&dir)
            .map_err(io_error(""))?
            .into_iter()
            .map(|(name, metadata)| {
                let writer = daemon.context.writer_of(project, &name);
                context::context_file(&name, &metadata, writer)
            })
            .collect();
        Ok(ContextListResult { files })
    })
    .await
}

pub(crate) async fn read(
    context: &Context,
    params: ContextReadParams,
) -> Result<ContextReadResult, ErrorObject> {
    let name = context::validate_relative_path(&params.path)?.to_owned();
    let project = params.project;
    ensure_project_exists(context, project).await?;
    let daemon = Arc::clone(&context.daemon);
    run_blocking(move || {
        let dir = context::ensure_dir(&daemon.data_dir, project).map_err(io_error(&name))?;
        let (bytes, metadata) = context::read_file(&dir, &name).map_err(io_error(&name))?;
        let text = String::from_utf8(bytes)
            .map_err(|_| ErrorObject::internal_error(format!("{name} is not valid UTF-8 text")))?;
        let writer = daemon.context.writer_of(project, &name);
        let file = context::context_file(&name, &metadata, writer);
        Ok(ContextReadResult {
            file,
            content: text,
        })
    })
    .await
}

/// The logic behind `context/write`, apart from wispd's blocking pool, so a test can call it
/// directly.
///
/// Idempotent on `id`: a retry with the same `project`, `path`, `content`, and `writer` returns
/// the file unchanged, without writing again or emitting a second `context.changed` event; a
/// retry with different params fails with `idConflict`. The last write to a path wins when two
/// race, since each runs independently and the later one's atomic rename is simply the one that
/// lands last (0005).
fn write_context_file(
    daemon: &crate::server::Daemon,
    project: ProjectId,
    name: &str,
    content: &str,
    writer: Option<String>,
    write_id: wisp_protocol::ContextWriteId,
) -> Result<ContextWriteResult, ErrorObject> {
    if content.len() as u64 > context::MAX_FILE_BYTES {
        return Err(ErrorObject::wisp(
            ErrorKind::ContextTooLarge,
            format!("{name} would be over the per-file shared context size cap"),
        ));
    }
    let dir = context::ensure_dir(&daemon.data_dir, project).map_err(io_error(name))?;
    match daemon.context.check(
        project,
        name,
        write_id,
        content.as_bytes(),
        writer.as_deref(),
    ) {
        Existing::SameRetry => {
            let (_, metadata) = context::read_file(&dir, name).map_err(io_error(name))?;
            let file = context::context_file(name, &metadata, writer);
            return Ok(ContextWriteResult { file });
        }
        Existing::Conflict => {
            return Err(ErrorObject::wisp(
                ErrorKind::IdConflict,
                format!("context write {write_id} exists with different content or writer"),
            ));
        }
        Existing::New => {}
    }
    let other_total = context::other_files_total(&dir, name).map_err(io_error(name))?;
    if other_total + content.len() as u64 > context::MAX_PROJECT_BYTES {
        return Err(ErrorObject::wisp(
            ErrorKind::ContextTooLarge,
            format!("{name} would be over the shared context size cap"),
        ));
    }
    let metadata = context::write_file(&dir, name, content.as_bytes()).map_err(io_error(name))?;
    daemon.context.record_protocol_write(
        project,
        name,
        write_id,
        writer.clone(),
        content.as_bytes(),
    );
    let file = context::context_file(name, &metadata, writer);
    let seq = daemon.log.append_blocking(
        jiff::Timestamp::now(),
        Some(project),
        WispEvent::ContextChanged { file: file.clone() },
    );
    info!(project = %project, path = name, seq, "wrote a shared context file");
    Ok(ContextWriteResult { file })
}

pub(crate) async fn write(
    context: &Context,
    params: ContextWriteParams,
) -> Result<ContextWriteResult, ErrorObject> {
    let ContextWriteParams {
        id,
        project,
        path,
        content,
        writer,
    } = params;
    let name = context::validate_relative_path(&path)?.to_owned();
    // A fast, redundant check: `write_context_file` enforces this cap too, but failing here skips
    // a project lookup and a trip to the blocking pool for a request that is invalid regardless.
    if content.len() as u64 > context::MAX_FILE_BYTES {
        return Err(ErrorObject::wisp(
            ErrorKind::ContextTooLarge,
            format!("{name} would be over the per-file shared context size cap"),
        ));
    }
    ensure_project_exists(context, project).await?;
    let daemon = Arc::clone(&context.daemon);
    run_blocking(move || write_context_file(&daemon, project, &name, &content, writer, id)).await
}

async fn ensure_project_exists(context: &Context, project: ProjectId) -> Result<(), ErrorObject> {
    context
        .daemon
        .store
        .run(&context.cancel, move |store| {
            let found = store
                .get_project(project.into())
                .map_err(|error| store_error(&error))?
                .is_some();
            if found {
                Ok(())
            } else {
                Err(store_error(&StoreError::NotFound { id: project.into() }))
            }
        })
        .await
}

/// Runs `job` on tokio's blocking pool, turning a panic or a dropped task into an internal error
/// rather than losing it silently.
async fn run_blocking<T, F>(job: F) -> Result<T, ErrorObject>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ErrorObject> + Send + 'static,
{
    match tokio::task::spawn_blocking(job).await {
        Ok(result) => result,
        Err(error) => Err(ErrorObject::internal_error(format!(
            "shared context task failed: {error}"
        ))),
    }
}

fn io_error(path: &str) -> impl Fn(std::io::Error) -> ErrorObject {
    let path = path.to_owned();
    move |error| context::io_error(&path, &error)
}

#[cfg(test)]
mod tests {
    use wisp_protocol::{ContextWriteId, ErrorKind, ProjectId};

    use super::write_context_file;
    use crate::server::Daemon;

    fn daemon() -> (tempfile::TempDir, std::sync::Arc<Daemon>) {
        let dir = tempfile::tempdir().unwrap();
        let daemon = Daemon::for_tests(dir.path(), 10, std::time::Duration::from_secs(90));
        (dir, daemon)
    }

    #[test]
    fn a_retry_with_the_same_params_is_idempotent_and_a_different_one_conflicts() {
        let (_dir, daemon) = daemon();
        let project = ProjectId::generate();
        let id = ContextWriteId::generate();

        let first = write_context_file(
            &daemon,
            project,
            "notes.md",
            "hello",
            Some("editor".to_owned()),
            id,
        )
        .unwrap();
        let retry = write_context_file(
            &daemon,
            project,
            "notes.md",
            "hello",
            Some("editor".to_owned()),
            id,
        )
        .unwrap();
        assert_eq!(first.file, retry.file);

        let conflict =
            write_context_file(&daemon, project, "notes.md", "different", None, id).unwrap_err();
        assert_eq!(conflict.wisp_data().unwrap().kind, ErrorKind::IdConflict);
    }

    #[test]
    fn a_fresh_id_overwrites_the_previous_content() {
        let (_dir, daemon) = daemon();
        let project = ProjectId::generate();

        write_context_file(
            &daemon,
            project,
            "notes.md",
            "first",
            None,
            ContextWriteId::generate(),
        )
        .unwrap();
        let second = write_context_file(
            &daemon,
            project,
            "notes.md",
            "second, and longer",
            None,
            ContextWriteId::generate(),
        )
        .unwrap();
        assert_eq!(second.file.size, "second, and longer".len() as u64);
    }

    #[test]
    fn a_file_over_the_per_file_cap_is_rejected() {
        let (_dir, daemon) = daemon();
        let project = ProjectId::generate();
        let too_big = "a".repeat(usize::try_from(super::context::MAX_FILE_BYTES).unwrap() + 1);
        let error = write_context_file(
            &daemon,
            project,
            "a.md",
            &too_big,
            None,
            ContextWriteId::generate(),
        )
        .unwrap_err();
        assert_eq!(error.wisp_data().unwrap().kind, ErrorKind::ContextTooLarge);
    }

    #[test]
    fn a_write_that_would_cross_the_per_project_cap_is_rejected() {
        let (_dir, daemon) = daemon();
        let project = ProjectId::generate();
        let one_file = "a".repeat(usize::try_from(super::context::MAX_FILE_BYTES).unwrap());
        write_context_file(
            &daemon,
            project,
            "a.md",
            &one_file,
            None,
            ContextWriteId::generate(),
        )
        .unwrap();

        // "a.md" already uses one file's worth of the cap; a second file that alone would use the
        // rest of it, plus one more byte, must push the project over the top.
        let remaining_plus_one = "b".repeat(
            usize::try_from(super::context::MAX_PROJECT_BYTES - super::context::MAX_FILE_BYTES + 1)
                .unwrap(),
        );
        let error = write_context_file(
            &daemon,
            project,
            "b.md",
            &remaining_plus_one,
            None,
            ContextWriteId::generate(),
        )
        .unwrap_err();
        assert_eq!(error.wisp_data().unwrap().kind, ErrorKind::ContextTooLarge);
    }
}
