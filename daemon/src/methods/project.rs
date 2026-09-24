//! `project/list` and `project/create`.

use std::path::Path;
use std::sync::Arc;

use tracing::info;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    ProjectCreateParams, ProjectCreateResult, ProjectListParams, ProjectListResult, WispEvent,
};

use super::Context;
use crate::store::{self, store_error};

/// Every project, oldest first, with the `seq` of the last event the list reflects.
pub(crate) async fn list(
    context: &Context,
    _: ProjectListParams,
) -> Result<ProjectListResult, ErrorObject> {
    let log = Arc::clone(&context.daemon.log);
    context
        .daemon
        .store
        .run(&context.cancel, move |store| {
            let rows = store.list_projects().map_err(|error| store_error(&error))?;
            let seq = log.head();
            let projects = rows
                .into_iter()
                .map(store::project)
                .collect::<Result<_, _>>()?;
            Ok(ProjectListResult { projects, seq })
        })
        .await
}

/// Creates a project, or returns the one that already has this id and these params.
///
/// `project.created` is appended only when the row is new. The store's thread is the only
/// writer, since the lock admits one wispd per data folder, so the lookup before the create can't
/// race another create.
pub(crate) async fn create(
    context: &Context,
    params: ProjectCreateParams,
) -> Result<ProjectCreateResult, ErrorObject> {
    check(&params)?;
    let log = Arc::clone(&context.daemon.log);
    context
        .daemon
        .store
        .run(&context.cancel, move |store| {
            let (id, fields) = store::fields(params);
            let existed = store
                .get_project(id)
                .map_err(|error| store_error(&error))?
                .is_some();
            let row = store
                .create_project(id, &fields)
                .map_err(|error| store_error(&error))?;
            let project = store::project(row)?;
            if !existed {
                let seq = log.append(
                    project.created_at,
                    None,
                    WispEvent::ProjectCreated {
                        project: project.clone(),
                    },
                );
                info!(project = %project.id, seq, "created a project");
            }
            Ok(ProjectCreateResult { project })
        })
        .await
}

/// The longest `name` wispd accepts, in bytes.
const MAX_NAME_BYTES: usize = 256;

/// The longest `repoPath` wispd accepts, in bytes: macOS's `PATH_MAX`.
const MAX_REPO_PATH_BYTES: usize = 1024;

// The limits also bound every `project.created` event, and with it the event log's memory, far
// below the frame limit.
fn check(params: &ProjectCreateParams) -> Result<(), ErrorObject> {
    let ProjectCreateParams {
        name, repo_path, ..
    } = params;
    if name.trim().is_empty() {
        return Err(ErrorObject::invalid_params("name must not be empty"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "name must be at most {MAX_NAME_BYTES} bytes"
        )));
    }
    if repo_path.len() > MAX_REPO_PATH_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "repoPath must be at most {MAX_REPO_PATH_BYTES} bytes"
        )));
    }
    if name.contains('\0') || repo_path.contains('\0') {
        return Err(ErrorObject::invalid_params(
            "name and repoPath must not contain NUL",
        ));
    }
    if !Path::new(repo_path).is_absolute() {
        return Err(ErrorObject::invalid_params(
            "repoPath must be an absolute path",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use wisp_protocol::jsonrpc::INVALID_PARAMS;
    use wisp_protocol::{ProjectCreateParams, ProjectId};

    use super::{MAX_NAME_BYTES, MAX_REPO_PATH_BYTES, check};

    fn params(name: &str, repo_path: &str) -> ProjectCreateParams {
        ProjectCreateParams {
            id: ProjectId::generate(),
            name: name.to_owned(),
            repo_path: repo_path.to_owned(),
        }
    }

    #[test]
    fn names_and_paths_have_byte_limits_and_no_nul() {
        let longest_name = "n".repeat(MAX_NAME_BYTES);
        let longest_path = format!("/{}", "p".repeat(MAX_REPO_PATH_BYTES - 1));
        assert!(check(&params(&longest_name, &longest_path)).is_ok());
        for (name, repo_path) in [
            (format!("{longest_name}n"), "/src".to_owned()),
            ("wisp".to_owned(), format!("{longest_path}p")),
            ("wi\0sp".to_owned(), "/src".to_owned()),
            ("wisp".to_owned(), "/x\0y".to_owned()),
        ] {
            let error = check(&params(&name, &repo_path)).unwrap_err();
            assert_eq!(error.code, INVALID_PARAMS, "{name:?} {repo_path:?}");
        }
    }

    #[test]
    fn names_must_not_be_blank_and_paths_must_be_absolute() {
        assert!(check(&params("wisp", "/src/wisp")).is_ok());
        for (name, repo_path) in [
            ("", "/src"),
            ("  ", "/src"),
            ("wisp", "src/wisp"),
            ("wisp", ""),
        ] {
            let error = check(&params(name, repo_path)).unwrap_err();
            assert_eq!(error.code, INVALID_PARAMS, "{name:?} {repo_path:?}");
        }
    }
}
