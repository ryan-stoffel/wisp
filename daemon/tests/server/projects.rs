//! `project/list`, `project/create`, and `project/update` against a real store.

use std::fs;
use std::path::Path;

use parallax_protocol::jsonrpc::{INTERNAL_ERROR, INVALID_PARAMS, Request};
use parallax_protocol::methods::{HostHealth, ProjectCreate, ProjectList, ProjectUpdate};
use parallax_protocol::{
    ErrorKind, HostHealthParams, ImageMediaType, Project, ProjectCreateParams, ProjectIcon,
    ProjectId, ProjectListParams, ProjectUpdateParams, ProjectUpdateResult, PromptImage,
    StoreState,
};
use rustix::process::Signal;
use serde_json::json;

use crate::support::{Client, Plxd, create_params, kind, temp_dir};

fn icon(name: &str, color: Option<&str>) -> ProjectIcon {
    ProjectIcon {
        name: name.to_owned(),
        color: color.map(str::to_owned),
        image: None,
    }
}

/// A 1x1 PNG in base64.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

fn image(media_type: ImageMediaType, data: &str) -> PromptImage {
    PromptImage {
        media_type,
        data: data.to_owned(),
    }
}

fn update(
    project: ProjectId,
    name: Option<&str>,
    icon: Option<ProjectIcon>,
) -> ProjectUpdateParams {
    ProjectUpdateParams {
        project,
        name: name.map(str::to_owned),
        icon,
        permission: None,
    }
}

#[tokio::test]
async fn projects_are_created_listed_and_retried_idempotently() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;

    let empty = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert!(empty.projects.is_empty());
    assert_eq!(empty.seq, 0);

    let params = create_params(dir.path(), "parallax");
    let created = client
        .call::<ProjectCreate>(params.clone())
        .await
        .unwrap()
        .project;
    assert_eq!(created.id, params.id);
    assert_eq!(created.name, params.name);
    assert_eq!(created.repo_path, params.repo_path);
    assert_eq!(created.created_at, created.updated_at);

    let retried = client
        .call::<ProjectCreate>(params.clone())
        .await
        .unwrap()
        .project;
    assert_eq!(
        retried, created,
        "a retry returns the stored project unchanged"
    );

    let second = client
        .call::<ProjectCreate>(create_params(dir.path(), "roster"))
        .await
        .unwrap()
        .project;
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects, [created, second], "oldest first");
    assert_eq!(
        listed.seq, 2,
        "one event per new project, none for the retry"
    );
}

#[tokio::test]
async fn a_create_that_reuses_an_id_with_other_params_is_an_id_conflict() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;
    let params = create_params(dir.path(), "parallax");
    let created = client
        .call::<ProjectCreate>(params.clone())
        .await
        .unwrap()
        .project;

    for conflicting in [
        ProjectCreateParams {
            name: "renamed".to_owned(),
            ..params.clone()
        },
        ProjectCreateParams {
            repo_path: "/elsewhere".to_owned(),
            ..params.clone()
        },
    ] {
        let error = client.call::<ProjectCreate>(conflicting).await.unwrap_err();
        assert_eq!(kind(&error), ErrorKind::IdConflict);
    }
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects, [created]);
}

#[tokio::test]
async fn a_create_with_an_icon_stores_it_and_a_retry_must_match_it() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;
    let params = ProjectCreateParams {
        icon: Some(icon("rocket", Some("green"))),
        ..create_params(dir.path(), "parallax")
    };
    let created = client
        .call::<ProjectCreate>(params.clone())
        .await
        .unwrap()
        .project;
    assert_eq!(created.icon, params.icon);
    assert_eq!(
        client
            .call::<ProjectCreate>(params.clone())
            .await
            .unwrap()
            .project,
        created,
        "an identical retry returns the project"
    );

    for other in [
        None,
        Some(icon("rocket", None)),
        Some(icon("rocket", Some("blue"))),
        Some(icon("star", Some("green"))),
    ] {
        let conflicting = ProjectCreateParams {
            icon: other,
            ..params.clone()
        };
        let error = client.call::<ProjectCreate>(conflicting).await.unwrap_err();
        assert_eq!(kind(&error), ErrorKind::IdConflict);
    }

    let invalid = ProjectCreateParams {
        icon: Some(icon("Rocket", None)),
        ..create_params(dir.path(), "roster")
    };
    let error = client.call::<ProjectCreate>(invalid).await.unwrap_err();
    assert_eq!(error.code, INVALID_PARAMS);

    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects, [created]);
    assert_eq!(listed.seq, 1);
}

#[tokio::test]
async fn projects_are_renamed_and_given_icons_without_moving_their_activity() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;
    let created = client
        .call::<ProjectCreate>(create_params(dir.path(), "parallax"))
        .await
        .unwrap()
        .project;
    assert_eq!(created.icon, None);

    let renamed = client
        .call::<ProjectUpdate>(update(created.id, Some("Parallax app"), None))
        .await
        .unwrap()
        .project;
    assert_eq!(
        renamed,
        Project {
            name: "Parallax app".to_owned(),
            ..created.clone()
        },
        "only the name changes: not the repository, the icon, or updatedAt"
    );

    let with_icon = client
        .call::<ProjectUpdate>(update(
            created.id,
            None,
            Some(icon("rocket", Some("green"))),
        ))
        .await
        .unwrap()
        .project;
    assert_eq!(
        with_icon,
        Project {
            icon: Some(icon("rocket", Some("green"))),
            ..renamed.clone()
        },
        "an absent name stays as it is"
    );

    let recolored = client
        .call::<ProjectUpdate>(update(created.id, None, Some(icon("rocket", None))))
        .await
        .unwrap()
        .project;
    assert_eq!(
        recolored.icon,
        Some(icon("rocket", None)),
        "the icon is replaced whole, so its color goes"
    );

    for unchanged in [
        update(created.id, None, None),
        update(created.id, Some("Parallax app"), Some(icon("rocket", None))),
    ] {
        let same = client
            .call::<ProjectUpdate>(unchanged)
            .await
            .unwrap()
            .project;
        assert_eq!(same, recolored);
    }
    client
        .send_message(&Request {
            id: "null-icon".into(),
            method: "project/update".to_owned(),
            params: Some(json!({"project": created.id, "icon": null})),
        })
        .await;
    let same: ProjectUpdateResult =
        serde_json::from_value(client.response().await.result.unwrap()).unwrap();
    assert_eq!(same.project, recolored, "a null icon reads as absent");

    assert_eq!(recolored.updated_at, created.updated_at);
    assert_eq!(recolored.repo_path, created.repo_path);
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects, [recolored]);
    assert_eq!(
        listed.seq, 4,
        "project.created and one project.updated per change, none for an update that changes nothing"
    );
}

/// An icon's image (PLX-339, 0038) is stored and listed, capped with `imageTooLarge`, checked
/// with `invalidParams`, and cleared by an icon sent without one.
#[tokio::test]
async fn an_icon_image_is_stored_capped_checked_and_cleared() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;
    let with_image = ProjectIcon {
        image: Some(image(ImageMediaType::Png, PNG)),
        ..icon("rocket", Some("green"))
    };
    let created = client
        .call::<ProjectCreate>(ProjectCreateParams {
            icon: Some(with_image.clone()),
            ..create_params(dir.path(), "parallax")
        })
        .await
        .unwrap()
        .project;
    assert_eq!(created.icon, Some(with_image.clone()));

    let too_large = format!("{PNG}{}", "A".repeat(64 * 1024));
    let error = client
        .call::<ProjectUpdate>(update(
            created.id,
            None,
            Some(ProjectIcon {
                image: Some(image(ImageMediaType::Png, &too_large)),
                ..icon("rocket", None)
            }),
        ))
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::ImageTooLarge);
    for bad in [
        image(ImageMediaType::Png, "not base64"),
        image(ImageMediaType::Webp, PNG),
    ] {
        let error = client
            .call::<ProjectUpdate>(update(
                created.id,
                None,
                Some(ProjectIcon {
                    image: Some(bad),
                    ..icon("rocket", None)
                }),
            ))
            .await
            .unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS, "{error:?}");
    }
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(
        listed.projects,
        std::slice::from_ref(&created),
        "a refused image changes nothing"
    );

    let cleared = client
        .call::<ProjectUpdate>(update(
            created.id,
            None,
            Some(icon("rocket", Some("green"))),
        ))
        .await
        .unwrap()
        .project;
    assert_eq!(cleared.icon, Some(icon("rocket", Some("green"))));
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects, [cleared]);
}

#[tokio::test]
async fn an_update_of_an_unknown_project_is_project_not_found() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;

    for params in [
        update(ProjectId::generate(), Some("parallax"), None),
        update(ProjectId::generate(), None, Some(icon("rocket", None))),
        update(ProjectId::generate(), None, None),
    ] {
        let error = client.call::<ProjectUpdate>(params).await.unwrap_err();
        assert_eq!(kind(&error), ErrorKind::ProjectNotFound);
    }
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert!(listed.projects.is_empty());
    assert_eq!(listed.seq, 0);
}

#[tokio::test]
async fn invalid_update_params_are_refused_before_the_store() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;
    let created = client
        .call::<ProjectCreate>(create_params(dir.path(), "parallax"))
        .await
        .unwrap()
        .project;

    let long_name = "n".repeat(257);
    let long_icon = "a".repeat(65);
    let long_color = "b".repeat(33);
    for params in [
        update(created.id, Some(""), None),
        update(created.id, Some("  "), None),
        update(created.id, Some(long_name.as_str()), None),
        update(created.id, Some("wi\0sp"), None),
        update(created.id, None, Some(icon("", None))),
        update(created.id, None, Some(icon("Rocket", None))),
        update(created.id, None, Some(icon("rocket ship", None))),
        update(created.id, None, Some(icon(&long_icon, None))),
        update(created.id, None, Some(icon("rocket", Some("")))),
        update(created.id, None, Some(icon("rocket", Some("#00ff00")))),
        update(
            created.id,
            None,
            Some(icon("rocket", Some(long_color.as_str()))),
        ),
        update(created.id, Some("roster"), Some(icon("Rocket", None))),
    ] {
        let error = client
            .call::<ProjectUpdate>(params.clone())
            .await
            .unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS, "{params:?}");
    }

    for (id, icon) in [
        (1_i64, json!("rocket")),
        (2, json!({"color": "green"})),
        (3, json!({"name": 7})),
        (4, json!({"name": "rocket", "color": 7})),
    ] {
        client
            .send_message(&Request {
                id: id.into(),
                method: "project/update".to_owned(),
                params: Some(json!({"project": created.id, "icon": icon})),
            })
            .await;
        let error = client.response().await.result.unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS, "{icon}");
    }
    // A Project runs only in Auto or Bypass (0042).
    client
        .send_message(&Request {
            id: 5.into(),
            method: "project/update".to_owned(),
            params: Some(json!({"project": created.id, "permission": "manual"})),
        })
        .await;
    let error = client.response().await.result.unwrap_err();
    assert_eq!(error.code, INVALID_PARAMS);

    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects, [created], "nothing changed");
    assert_eq!(listed.seq, 1);
}

#[tokio::test]
async fn invalid_create_params_are_refused_before_the_store() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;

    client
        .send_message(&Request {
            id: 1.into(),
            method: "project/create".to_owned(),
            params: Some(json!({"id": "not-a-uuid", "name": "parallax", "repoPath": "/src"})),
        })
        .await;
    let error = client.response().await.result.unwrap_err();
    assert_eq!(error.code, INVALID_PARAMS);
    assert!(error.message.contains("UUIDv7"), "{}", error.message);

    let long_path = format!("/{}", "p".repeat(1024));
    for (name, repo_path) in [
        ("parallax", "relative/path"),
        (" ", "/src"),
        ("parallax", "/x\0y"),
        ("wi\0sp", "/src"),
        (&"n".repeat(257), "/src"),
        ("parallax", &long_path),
    ] {
        let params = ProjectCreateParams {
            name: name.to_owned(),
            repo_path: repo_path.to_owned(),
            ..create_params(dir.path(), "x")
        };
        let error = client.call::<ProjectCreate>(params).await.unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS, "{name:?} {repo_path:?}");
    }
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert!(listed.projects.is_empty());

    // Params at the limits pass the checks and reach the repository check, which this path fails.
    let at_the_limits = ProjectCreateParams {
        name: "n".repeat(256),
        repo_path: long_path[..1024].to_owned(),
        ..create_params(dir.path(), "x")
    };
    let error = client
        .call::<ProjectCreate>(at_the_limits)
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::NotARepository);
}

#[tokio::test]
async fn a_new_project_needs_a_repository_and_reports_its_branch() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;

    let plain = dir.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let missing = dir.path().join("missing");
    for (path, reason) in [
        (&plain, "is not the top folder of a git repository"),
        (&missing, "doesn't exist on this host"),
    ] {
        let params = ProjectCreateParams {
            repo_path: path.to_str().unwrap().to_owned(),
            ..create_params(dir.path(), "parallax")
        };
        let error = client.call::<ProjectCreate>(params).await.unwrap_err();
        assert_eq!(kind(&error), ErrorKind::NotARepository);
        assert!(error.message.contains(reason), "{}", error.message);
    }
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert!(listed.projects.is_empty(), "nothing was created");
    assert_eq!(listed.seq, 0);

    let params = create_params(dir.path(), "parallax");
    let created = client
        .call::<ProjectCreate>(params.clone())
        .await
        .unwrap()
        .project;
    assert_eq!(created.branch.as_deref(), Some("main"));

    let head = Path::new(&params.repo_path).join(".git").join("HEAD");
    fs::write(&head, "ref: refs/heads/feature/104\n").unwrap();
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(
        listed.projects[0].branch.as_deref(),
        Some("feature/104"),
        "read when listed"
    );

    fs::remove_dir_all(&params.repo_path).unwrap();
    let retried = client
        .call::<ProjectCreate>(params.clone())
        .await
        .expect("a retry returns the project after its folder is gone");
    assert_eq!(retried.project.id, created.id);
    assert_eq!(retried.project.branch, None);
}

#[tokio::test]
async fn projects_and_the_event_log_outlive_a_restart() {
    let dir = temp_dir();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::connect(&plxd.socket).await;
    let first_log = client.initialize().await.unwrap().log_id;
    let created = client
        .call::<ProjectCreate>(create_params(dir.path(), "parallax"))
        .await
        .unwrap()
        .project;
    drop(client);
    plxd.signal(Signal::TERM);
    assert!(plxd.exit().await.0.success());

    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::connect(&plxd.socket).await;
    let second_log = client.initialize().await.unwrap().log_id;
    assert_eq!(
        first_log, second_log,
        "the log is stored from M3 on, so it continues (decision 0014)"
    );
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects, [created]);
    assert_eq!(listed.seq, 1, "project.created is still in the log");
}

#[tokio::test]
async fn a_store_that_cannot_open_is_reported_and_project_methods_fail() {
    let dir = temp_dir();
    fs::create_dir(dir.path().join("plxd.sqlite3")).unwrap();
    let plxd = Plxd::start(dir.path()).await;
    let mut client = Client::ready(&plxd.socket).await;

    let health = client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
    assert_eq!(health.store, StoreState::Unavailable);
    let error = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap_err();
    assert_eq!(error.code, INTERNAL_ERROR);
    assert_eq!(
        error.message,
        "Internal error: the project store is unavailable"
    );
}
