//! `project/list`, `project/create`, `project/start`, which starts a project's coordinator behind
//! the `coordinator` capability (0024), and `project/update`, which renames a project or sets its
//! icon behind the `projectEdit` capability (RYA-227, 0032) or its permission mode behind
//! `projectPermission` (0042), and `project/delete`, behind `projectDelete` (PLX-338).

use std::path::{Component, Path};
use std::sync::Arc;

use jiff::Timestamp;
use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::{
    AgentRunResult, ErrorKind, ParallaxEvent, ProjectCreateParams, ProjectCreateResult,
    ProjectDeleteParams, ProjectDeleteResult, ProjectIcon, ProjectId, ProjectListParams,
    ProjectListResult, ProjectPermission, ProjectStartParams, ProjectUpdateParams,
    ProjectUpdateResult, RunId,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::Context;
use crate::agents::{self, coordinator};
use crate::repo;
use crate::server::Daemon;
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
                .map(|row| {
                    let coordinator = coordinator::coordinator_of(store, row.id)?;
                    store::project(row, coordinator)
                })
                .collect::<Result<_, _>>()?;
            Ok(ProjectListResult { projects, seq })
        })
        .await
}

/// Creates a project, or returns the one that already has this id and these params.
///
/// `project.created` is appended only when the row is new. The store's thread is the only
/// writer, since the lock admits one plxd per data folder, so the lookup before the create can't
/// race another create.
///
/// Only a new project's `repoPath` has to be a repository, so a retry still returns the project
/// after its folder is gone.
pub(crate) async fn create(
    context: &Context,
    params: ProjectCreateParams,
) -> Result<ProjectCreateResult, ErrorObject> {
    check(&params)?;
    let log = Arc::clone(&context.daemon.log);
    let data_dir = context.daemon.data_dir.clone();
    context
        .daemon
        .store
        .run(&context.cancel, move |store| {
            let (id, fields) = store::fields(params);
            let existed = store
                .get_project(id)
                .map_err(|error| store_error(&error))?
                .is_some();
            if !existed {
                repo::check(Path::new(&fields.repo_path)).map_err(|error| {
                    ErrorObject::parallax(ErrorKind::NotARepository, error.to_string())
                })?;
            }
            let row = store
                .create_project(id, &fields)
                .map_err(|error| store_error(&error))?;
            let coordinator = coordinator::coordinator_of(store, id)?;
            let project = store::project(row, coordinator)?;
            if !existed {
                let seq = log.append_blocking(
                    project.created_at,
                    None,
                    ParallaxEvent::ProjectCreated {
                        project: project.clone(),
                    },
                );
                info!(project = %project.id, seq, "created a project");
                // Best effort (#155): a project's shared context folder is also ensured lazily on
                // its first `context/*` call, so a failure here never blocks project creation.
                if let Err(error) = crate::context::ensure_dir(&data_dir, project.id) {
                    warn!(project = %project.id, %error, "could not create the shared context folder");
                }
            }
            Ok(ProjectCreateResult { project })
        })
        .await
}

/// Starts the project's coordinator, detached from the request as `agent/start` is, so a dropped
/// connection never leaves it half started.
pub(crate) async fn start(
    context: &Context,
    params: ProjectStartParams,
) -> Result<AgentRunResult, ErrorObject> {
    super::agent::check_message("prompt", &params.prompt, &params.images)?;
    let daemon = Arc::clone(&context.daemon);
    let run = context
        .daemon
        .agents
        .detached(coordinator::start(daemon, params))
        .await?;
    Ok(AgentRunResult { run })
}

/// Renames a project or sets its icon or permission mode, and appends `project.updated` when that
/// changed anything. Runs pick up a new mode when they next start a CLI process (0042).
///
/// The event is appended in the job that writes the row, as `project/create`'s is, so a
/// `project/list` snapshot and its `seq` always agree. `updatedAt` stays as it is (0032).
pub(crate) async fn update(
    context: &Context,
    params: ProjectUpdateParams,
) -> Result<ProjectUpdateResult, ErrorObject> {
    if let Some(name) = &params.name {
        check_name(name)?;
    }
    if let Some(icon) = &params.icon {
        check_icon(icon)?;
    }
    check_permission(params.permission)?;
    let log = Arc::clone(&context.daemon.log);
    context
        .daemon
        .store
        .run(&context.cancel, move |store| {
            let (id, edit) = store::edit(params);
            let (row, changed) = store
                .update_project(id, &edit)
                .map_err(|error| store_error(&error))?;
            let coordinator = coordinator::coordinator_of(store, id)?;
            let project = store::project(row, coordinator)?;
            if changed {
                let seq = log.append_blocking(
                    Timestamp::now(),
                    None,
                    ParallaxEvent::ProjectUpdated {
                        project: project.clone(),
                    },
                );
                info!(project = %project.id, seq, "updated a project");
            }
            Ok(ProjectUpdateResult { project })
        })
        .await
}

/// Deletes a project (PLX-338), detached from the request as `thread/delete` is, so a dropped
/// connection never leaves it half deleted.
pub(crate) async fn delete(
    context: &Context,
    params: ProjectDeleteParams,
) -> Result<ProjectDeleteResult, ErrorObject> {
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(remove(daemon, params.project))
        .await
}

/// Deletes each of `project`'s runs through its actor, as `thread/delete` deletes a thread's,
/// coordinators first so none starts another run meanwhile. Then, in one store job once no run is
/// left, deletes the project's row and appends `project.deleted`, and last removes its shared
/// context folder. It looks again after each pass, for a run a coordinator started while it was
/// stopping; a worker is recorded only while its scope exists, so none can start after the row
/// is gone. A crash midway leaves the project listed, and deleting it again finishes the job.
async fn remove(
    daemon: Arc<Daemon>,
    project: ProjectId,
) -> Result<ProjectDeleteResult, ErrorObject> {
    loop {
        let log = Arc::clone(&daemon.log);
        let runs = daemon
            .store
            .run(&CancellationToken::new(), move |store| {
                if store
                    .get_project(project.into())
                    .map_err(|error| store_error(&error))?
                    .is_none()
                {
                    return Err(ErrorObject::parallax(
                        ErrorKind::ProjectNotFound,
                        format!("no project has id {project}"),
                    ));
                }
                let mut runs = store
                    .list_runs(Some(project.into()))
                    .map_err(|error| store_error(&error))?;
                if runs.is_empty() {
                    store
                        .delete_project(project.into())
                        .map_err(|error| store_error(&error))?;
                    let seq = log.append_blocking(
                        Timestamp::now(),
                        None,
                        ParallaxEvent::ProjectDeleted { project },
                    );
                    info!(%project, seq, "deleted a project");
                }
                // A coordinator's thread is its own run (0024).
                runs.sort_by_key(|run| run.fields.coordinator_thread != Some(run.id));
                runs.into_iter()
                    .map(|run| {
                        RunId::try_from(run.id).map_err(|_| {
                            ErrorObject::internal_error(format!(
                                "the stored run {} has an invalid id",
                                run.id
                            ))
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .await?;
        if runs.is_empty() {
            break;
        }
        for run in runs {
            match agents::delete(&daemon, run).await {
                Ok(()) => {}
                // Another `project/delete` got to it first.
                Err(error)
                    if error
                        .parallax_data()
                        .is_some_and(|data| data.kind == ErrorKind::RunNotFound) => {}
                Err(error) => return Err(error),
            }
        }
    }
    crate::threads::remove_context(&daemon, project);
    Ok(ProjectDeleteResult {})
}

/// The longest `name` plxd accepts, in bytes.
const MAX_NAME_BYTES: usize = 256;

/// The longest `repoPath` plxd accepts, in bytes: macOS's `PATH_MAX`.
const MAX_REPO_PATH_BYTES: usize = 1024;

/// The longest icon name plxd accepts, in characters (0032).
const MAX_ICON_NAME_CHARS: usize = 64;

/// The longest icon color plxd accepts, in characters (0032).
const MAX_ICON_COLOR_CHARS: usize = 32;

// The limits also bound every `project.created` and `project.updated` event, and with it the
// event log's memory, far below the frame limit.
fn check(params: &ProjectCreateParams) -> Result<(), ErrorObject> {
    let ProjectCreateParams {
        name,
        repo_path,
        icon,
        permission,
        ..
    } = params;
    check_name(name)?;
    check_permission(*permission)?;
    if let Some(icon) = icon {
        check_icon(icon)?;
    }
    if repo_path.len() > MAX_REPO_PATH_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "repoPath must be at most {MAX_REPO_PATH_BYTES} bytes"
        )));
    }
    if repo_path.contains('\0') {
        return Err(ErrorObject::invalid_params("repoPath must not contain NUL"));
    }
    if !Path::new(repo_path).is_absolute() {
        return Err(ErrorObject::invalid_params(
            "repoPath must be an absolute path",
        ));
    }
    // One folder has one spelling, so the stored path names the folder that was checked and a
    // retry spelled differently is a conflict. `components()` drops a `.` inside the path, so
    // the text is checked for those too.
    let has_dot_segment = Path::new(repo_path)
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        || repo_path.contains("/./")
        || repo_path.ends_with("/.");
    if has_dot_segment {
        return Err(ErrorObject::invalid_params(
            "repoPath must not contain . or .. segments",
        ));
    }
    Ok(())
}

/// The rules for a project's name, in `project/create` and `project/update` alike.
fn check_name(name: &str) -> Result<(), ErrorObject> {
    if name.trim().is_empty() {
        return Err(ErrorObject::invalid_params("name must not be empty"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "name must be at most {MAX_NAME_BYTES} bytes"
        )));
    }
    if name.contains('\0') {
        return Err(ErrorObject::invalid_params("name must not contain NUL"));
    }
    Ok(())
}

/// A project runs in Auto or Bypass Permissions (0042), so a mode this plxd doesn't know is refused.
fn check_permission(permission: Option<ProjectPermission>) -> Result<(), ErrorObject> {
    if permission == Some(ProjectPermission::Unknown) {
        return Err(ErrorObject::invalid_params(
            "permission must be auto or bypass",
        ));
    }
    Ok(())
}

/// An icon's name and color are keys of `a-z`, `0-9`, and `-` (0032). plxd never reads them, so
/// that is all it checks. Its image is capped and checked as a prompt's are (0038).
pub(crate) fn check_icon(icon: &ProjectIcon) -> Result<(), ErrorObject> {
    check_key("icon.name", &icon.name, MAX_ICON_NAME_CHARS)?;
    if let Some(color) = &icon.color {
        check_key("icon.color", color, MAX_ICON_COLOR_CHARS)?;
    }
    if let Some(image) = &icon.image {
        crate::images::check_icon(image)?;
    }
    Ok(())
}

fn check_key(field: &str, key: &str, max: usize) -> Result<(), ErrorObject> {
    let allowed = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-';
    if (1..=max).contains(&key.len()) && key.bytes().all(allowed) {
        Ok(())
    } else {
        Err(ErrorObject::invalid_params(format!(
            "{field} must be 1 to {max} characters of a-z, 0-9, and -"
        )))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use parallax_protocol::jsonrpc::INVALID_PARAMS;
    use parallax_protocol::{ProjectCreateParams, ProjectIcon, ProjectId};

    use super::{
        MAX_ICON_COLOR_CHARS, MAX_ICON_NAME_CHARS, MAX_NAME_BYTES, MAX_REPO_PATH_BYTES, check,
        check_icon,
    };

    fn params(name: &str, repo_path: &str) -> ProjectCreateParams {
        ProjectCreateParams {
            id: ProjectId::generate(),
            name: name.to_owned(),
            repo_path: repo_path.to_owned(),
            icon: None,
            permission: None,
        }
    }

    fn icon(name: &str, color: Option<&str>) -> ProjectIcon {
        ProjectIcon {
            name: name.to_owned(),
            color: color.map(str::to_owned),
            image: None,
        }
    }

    #[test]
    fn icons_are_short_keys_of_lowercase_letters_digits_and_hyphens() {
        let longest_name = "a".repeat(MAX_ICON_NAME_CHARS);
        let longest_color = "b".repeat(MAX_ICON_COLOR_CHARS);
        for valid in [
            icon("rocket", None),
            icon("folder-kanban", Some("green")),
            icon("x", Some("2")),
            icon("arrow-up-01", Some("sky-500")),
            icon(&longest_name, Some(&longest_color)),
        ] {
            assert!(check_icon(&valid).is_ok(), "{valid:?}");
        }
        for invalid in [
            icon("", None),
            icon("Rocket", None),
            icon("rocket ship", None),
            icon("rocket_ship", None),
            icon("rocket\0", None),
            icon("ra\u{301}cket", None),
            icon(&format!("{longest_name}a"), None),
            icon("rocket", Some("")),
            icon("rocket", Some("Green")),
            icon("rocket", Some("#00ff00")),
            icon("rocket", Some(&format!("{longest_color}b"))),
        ] {
            let error = check_icon(&invalid).unwrap_err();
            assert_eq!(error.code, INVALID_PARAMS, "{invalid:?}");
        }
        let error = check(&ProjectCreateParams {
            icon: Some(icon("Rocket", None)),
            ..params("parallax", "/src/parallax")
        })
        .unwrap_err();
        assert_eq!(
            error.message,
            "Invalid params: icon.name must be 1 to 64 characters of a-z, 0-9, and -"
        );
    }

    #[test]
    fn names_and_paths_have_byte_limits_and_no_nul() {
        let longest_name = "n".repeat(MAX_NAME_BYTES);
        let longest_path = format!("/{}", "p".repeat(MAX_REPO_PATH_BYTES - 1));
        assert!(check(&params(&longest_name, &longest_path)).is_ok());
        for (name, repo_path) in [
            (format!("{longest_name}n"), "/src".to_owned()),
            ("parallax".to_owned(), format!("{longest_path}p")),
            ("wi\0sp".to_owned(), "/src".to_owned()),
            ("parallax".to_owned(), "/x\0y".to_owned()),
        ] {
            let error = check(&params(&name, &repo_path)).unwrap_err();
            assert_eq!(error.code, INVALID_PARAMS, "{name:?} {repo_path:?}");
        }
    }

    #[test]
    fn names_must_not_be_blank_and_paths_must_be_absolute_without_dot_segments() {
        assert!(check(&params("parallax", "/src/parallax")).is_ok());
        for (name, repo_path) in [
            ("", "/src"),
            ("  ", "/src"),
            ("parallax", "src/parallax"),
            ("parallax", ""),
            ("parallax", "/src/../etc"),
            ("parallax", "/src/./app"),
            ("parallax", "/src/app/.."),
            ("parallax", "/src/app/."),
        ] {
            let error = check(&params(name, repo_path)).unwrap_err();
            assert_eq!(error.code, INVALID_PARAMS, "{name:?} {repo_path:?}");
        }
    }
}
