//! `initialize` and the host methods.

use std::collections::BTreeMap;

use serde_json::json;
use wisp_protocol::framing::MAX_FRAME_BYTES;
use wisp_protocol::jsonrpc::{INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, Request};
use wisp_protocol::methods::{HostHealth, HostVersion, Initialize, ProjectList, RequestMethod};
use wisp_protocol::{
    Capabilities, ErrorKind, HostHealthParams, HostVersionParams, IncompatibleProtocolDetail,
    ProjectListParams, ProtocolRange, StoreState,
};
use wispd::VERSION;

use crate::support::{Client, Wispd, kind, temp_dir};

#[tokio::test]
async fn the_handshake_agrees_on_a_version_and_reports_the_host() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::connect(&wispd.socket).await;

    let init = client.initialize().await.unwrap();
    assert_eq!(init.protocol, 1);
    assert_eq!(init.wispd, VERSION);
    assert_eq!(
        init.capabilities,
        Capabilities(BTreeMap::from([(
            "agentClis".to_owned(),
            serde_json::Map::new()
        )]))
    );
    assert_eq!(init.max_frame_bytes, MAX_FRAME_BYTES as u64);

    let health = client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
    assert_eq!(health.store, StoreState::Ok);
    assert_eq!(health.running_agents, 0);
    assert!(health.uptime_seconds < 60);

    let version = client
        .call::<HostVersion>(HostVersionParams {})
        .await
        .unwrap();
    assert_eq!(version.wispd, VERSION);
    assert_eq!(version.protocol, ProtocolRange::SUPPORTED);
    assert_eq!(version.arch, std::env::consts::ARCH);
    assert!(version.os.starts_with("macOS "), "{}", version.os);
}

#[tokio::test]
async fn requests_before_initialize_get_not_initialized() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::connect(&wispd.socket).await;

    let error = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::NotInitialized);
    assert_eq!(error.message, "initialize must be the first request");

    client.initialize().await.unwrap();
    client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
}

#[tokio::test]
async fn a_version_mismatch_gets_incompatible_protocol_and_the_connection_stays_usable() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::connect(&wispd.socket).await;

    let error = client
        .initialize_with(ProtocolRange { min: 2, max: 3 })
        .await
        .unwrap_err();
    let data = error.wisp_data().expect("a wisp error");
    assert_eq!(data.kind, ErrorKind::IncompatibleProtocol);
    let detail: IncompatibleProtocolDetail =
        serde_json::from_value(data.detail.expect("the frozen detail")).unwrap();
    assert_eq!(
        detail,
        IncompatibleProtocolDetail {
            requested: ProtocolRange { min: 2, max: 3 },
            supported: ProtocolRange::SUPPORTED,
            wispd: VERSION.to_owned(),
        }
    );

    let still_uninitialized = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap_err();
    assert_eq!(kind(&still_uninitialized), ErrorKind::NotInitialized);
    assert_eq!(client.initialize().await.unwrap().protocol, 1);
}

#[tokio::test]
async fn a_newer_client_is_answered_at_the_highest_common_version() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::connect(&wispd.socket).await;

    // A future client: a wider range, and fields and capabilities this version doesn't know.
    client
        .send_message(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocol": {"min": 1, "max": 4},
                "client": {"name": "wisp", "version": "9.0.0", "locale": "en"},
                "capabilities": {"agents": {}, "someday": {"level": 3}},
                "trace": "off"
            }
        }))
        .await;
    let response = client.response().await;
    let init: wisp_protocol::InitializeResult = response.into_result().unwrap();
    assert_eq!(init.protocol, 1);
}

#[tokio::test]
async fn a_second_initialize_is_an_invalid_request() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

    let error = client.initialize().await.unwrap_err();
    assert_eq!(error.code, INVALID_REQUEST);
}

#[tokio::test]
async fn bad_initialize_params_are_invalid_params() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::connect(&wispd.socket).await;

    client
        .send_message(&Request {
            id: 42.into(),
            method: Initialize::NAME.to_owned(),
            params: Some(json!({"protocol": "one"})),
        })
        .await;
    let response = client.response().await;
    assert_eq!(response.id, Some(42.into()));
    assert_eq!(response.result.unwrap_err().code, INVALID_PARAMS);
    client.initialize().await.unwrap();
}

#[tokio::test]
async fn unknown_methods_are_not_found() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

    client
        .send_message(&Request {
            id: 41.into(),
            method: "example/missing".to_owned(),
            params: Some(json!({})),
        })
        .await;
    let response = client.response().await;
    assert_eq!(response.id, Some(41.into()));
    assert_eq!(response.result.unwrap_err().code, METHOD_NOT_FOUND);
}
