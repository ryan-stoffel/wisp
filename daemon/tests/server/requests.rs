//! Framing, cancellation, and connections that end in the middle of things.

use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::sleep;
use wisp_protocol::framing::MAX_FRAME_BYTES;
use wisp_protocol::jsonrpc::{
    CancelRequestParams, INVALID_REQUEST, Message, PARSE_ERROR, REQUEST_CANCELLED, Response,
};
use wisp_protocol::methods::{CancelRequest, HostHealth, Initialize, ProjectCreate, ProjectList};
use wisp_protocol::{
    HostHealthParams, InitializeResult, ProjectCreateResult, ProjectListParams, ProjectListResult,
};

use crate::support::{Client, Wispd, WriteLock, create_params, temp_dir};

// Long enough for a request to reach the store's thread, which takes microseconds.
const SETTLE: Duration = Duration::from_millis(300);

#[tokio::test]
async fn a_cancelled_request_gets_exactly_one_answer_and_started_work_finishes() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    let lock = WriteLock::take(dir.path());

    // The create starts and waits for SQLite's lock; the list queues behind it.
    let params = create_params(dir.path(), "wisp");
    let create = client.send::<ProjectCreate>(params.clone()).await;
    sleep(SETTLE).await;
    let list = client.send::<ProjectList>(ProjectListParams {}).await;
    client
        .notify::<CancelRequest>(CancelRequestParams { id: list.clone() })
        .await;
    let cancelled = client.response().await;
    assert_eq!(cancelled.id, Some(list));
    assert_eq!(cancelled.result.unwrap_err().code, REQUEST_CANCELLED);

    // Cancelling the create, which already started, doesn't stop it.
    client
        .notify::<CancelRequest>(CancelRequestParams { id: create.clone() })
        .await;
    client.stays_quiet(SETTLE).await;
    lock.release();
    let finished = client.response().await;
    assert_eq!(finished.id, Some(create));
    let created: ProjectCreateResult = finished.into_result().unwrap();
    assert_eq!(created.project.id, params.id);

    // A cancel for a request that was already answered is ignored.
    client
        .notify::<CancelRequest>(CancelRequestParams { id: 1.into() })
        .await;
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects.len(), 1);
}

#[tokio::test]
async fn an_id_already_in_flight_is_refused() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    let lock = WriteLock::take(dir.path());

    let request = |id: i64| {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "project/create",
            "params": create_params(dir.path(), "wisp"),
        })
    };
    client.send_message(&request(7)).await;
    sleep(SETTLE).await;
    client.send_message(&request(7)).await;
    let refused = client.response().await;
    assert_eq!(refused.id, Some(7.into()));
    assert_eq!(refused.result.unwrap_err().code, INVALID_REQUEST);

    lock.release();
    let answered = client.response().await;
    assert_eq!(answered.id, Some(7.into()));
    assert!(answered.result.is_ok());
}

#[tokio::test]
async fn malformed_lines_are_answered_and_the_connection_stays_open() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

    client.send_raw(b"not json\n").await.unwrap();
    let parse = client.response().await;
    assert_eq!(parse.id, None);
    assert_eq!(parse.result.unwrap_err().code, PARSE_ERROR);

    client
        .send_raw(b"[]\n{\"jsonrpc\":\"2.0\",\"id\":9,\"method\":\"host/health\",\"params\":5}\n")
        .await
        .unwrap();
    for id in [None, Some(9.into())] {
        let invalid = client.response().await;
        assert_eq!(invalid.id, id);
        assert_eq!(invalid.result.unwrap_err().code, INVALID_REQUEST);
    }

    // Responses from the client and unknown notifications are ignored.
    client
        .send_raw(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n{\"jsonrpc\":\"2.0\",\"method\":\"$/progress\"}\n")
        .await
        .unwrap();
    client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
}

#[tokio::test]
async fn an_oversized_frame_closes_the_connection() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

    // The server may close before it has read everything, so the write can fail.
    let _ = client.send_raw(&vec![b' '; MAX_FRAME_BYTES + 1]).await;
    assert!(client.closes_within(Duration::from_secs(5)).await);

    let mut other = Client::ready(&wispd.socket).await;
    other.call::<HostHealth>(HostHealthParams {}).await.unwrap();
}

#[tokio::test]
async fn requests_sent_before_the_client_closes_its_side_are_all_answered() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::connect(&wispd.socket).await;

    // As `printf '...' | wispd attach` would: pipelined requests, then end of input.
    let init = client
        .send::<Initialize>(
            serde_json::from_value(json!({
                "protocol": {"min": 1, "max": 1},
                "client": {"name": "printf", "version": "0"},
                "capabilities": {}
            }))
            .unwrap(),
        )
        .await;
    let create = client
        .send::<ProjectCreate>(create_params(dir.path(), "wisp"))
        .await;
    let list = client.send::<ProjectList>(ProjectListParams {}).await;
    client.close_write().await;

    let mut answers = Vec::new();
    while let Some(message) = client.next().await {
        let Message::Response(response) = message else {
            panic!("expected only responses, got {message:?}");
        };
        answers.push(response);
    }
    let answer = |id| {
        answers
            .iter()
            .find(|response: &&Response| response.id.as_ref() == Some(id))
            .cloned()
            .unwrap_or_else(|| panic!("no answer for {id}"))
            .result
            .expect("a successful answer")
    };
    assert_eq!(answers.len(), 3, "{answers:?}");
    serde_json::from_value::<InitializeResult>(answer(&init)).unwrap();
    serde_json::from_value::<ProjectCreateResult>(answer(&create)).unwrap();
    serde_json::from_value::<ProjectListResult>(answer(&list)).unwrap();
}

#[tokio::test]
async fn a_client_that_disconnects_mid_request_leaves_the_server_serving() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;

    // Half a frame, then gone.
    let mut partial = Client::ready(&wispd.socket).await;
    partial
        .send_raw(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"host/hea")
        .await
        .unwrap();
    drop(partial);

    // A whole create, then gone before the answer.
    let params = create_params(dir.path(), "wisp");
    let mut hasty = Client::ready(&wispd.socket).await;
    hasty.send::<ProjectCreate>(params.clone()).await;
    drop(hasty);

    // The retry with the same id is safe whether or not the first create ran.
    let mut client = Client::ready(&wispd.socket).await;
    let retried = client
        .call::<ProjectCreate>(params.clone())
        .await
        .unwrap()
        .project;
    assert_eq!(retried.id, params.id);
    let listed = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    assert_eq!(listed.projects, [retried]);
    assert_eq!(listed.seq, 1);
    client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
}

#[tokio::test]
async fn params_may_be_absent_or_null() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    for params in [None, Some(Value::Null)] {
        let mut request = json!({"jsonrpc": "2.0", "id": 3, "method": "host/health"});
        if let Some(params) = params {
            request["params"] = params;
        }
        client.send_message(&request).await;
        assert!(client.response().await.result.is_ok());
    }
}
