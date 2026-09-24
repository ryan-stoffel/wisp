//! `project/list` and `project/create` against a real store.

use std::fs;

use rustix::process::Signal;
use serde_json::json;
use wisp_protocol::jsonrpc::{INTERNAL_ERROR, INVALID_PARAMS, Request};
use wisp_protocol::methods::{HostHealth, ProjectCreate, ProjectList};
use wisp_protocol::{
    ErrorKind, HostHealthParams, ProjectCreateParams, ProjectListParams, StoreState,
};

use crate::support::{Client, Wispd, create_params, kind, temp_dir};

#[tokio::test]
async fn projects_are_created_listed_and_retried_idempotently() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

    let empty = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert!(empty.projects.is_empty());
    assert_eq!(empty.seq, 0);

    let params = create_params("wisp");
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
        .call::<ProjectCreate>(create_params("roster"))
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
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    let params = create_params("wisp");
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
async fn invalid_create_params_are_refused_before_the_store() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

    client
        .send_message(&Request {
            id: 1.into(),
            method: "project/create".to_owned(),
            params: Some(json!({"id": "not-a-uuid", "name": "wisp", "repoPath": "/src"})),
        })
        .await;
    let error = client.response().await.result.unwrap_err();
    assert_eq!(error.code, INVALID_PARAMS);
    assert!(error.message.contains("UUIDv7"), "{}", error.message);

    for (name, repo_path) in [("wisp", "relative/path"), (" ", "/src")] {
        let params = ProjectCreateParams {
            name: name.to_owned(),
            repo_path: repo_path.to_owned(),
            ..create_params("x")
        };
        let error = client.call::<ProjectCreate>(params).await.unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS, "{name:?} {repo_path:?}");
    }
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert!(listed.projects.is_empty());
}

#[tokio::test]
async fn projects_outlive_a_restart_and_the_event_log_starts_over() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::connect(&wispd.socket).await;
    let first_log = client.initialize().await.unwrap().log_id;
    let created = client
        .call::<ProjectCreate>(create_params("wisp"))
        .await
        .unwrap()
        .project;
    drop(client);
    wispd.signal(Signal::TERM);
    assert!(wispd.exit().await.0.success());

    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::connect(&wispd.socket).await;
    let second_log = client.initialize().await.unwrap().log_id;
    assert_ne!(
        first_log, second_log,
        "M1's log is in memory, so it starts over"
    );
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects, [created]);
    assert_eq!(listed.seq, 0);
}

#[tokio::test]
async fn a_store_that_cannot_open_is_reported_and_project_methods_fail() {
    let dir = temp_dir();
    fs::create_dir(dir.path().join("wispd.sqlite3")).unwrap();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

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
