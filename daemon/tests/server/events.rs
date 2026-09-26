//! `events/subscribe`, `events/event`, and `events/unsubscribe`.

use std::path::Path;
use std::time::Duration;

use wisp_protocol::jsonrpc::Message;
use wisp_protocol::methods::{
    EventsSubscribe, EventsUnsubscribe, HostHealth, ProjectCreate, ProjectList,
};
use wisp_protocol::{
    ErrorKind, EventsEventParams, EventsSubscribeParams, EventsUnsubscribeParams, HostHealthParams,
    Project, ProjectId, ProjectListParams, SubscriptionId, WispEvent,
};

use crate::support::{Client, InProcess, Wispd, create_params, kind, temp_dir};

async fn create(client: &mut Client, dir: &Path, name: &str) -> Project {
    client
        .call::<ProjectCreate>(create_params(dir, name))
        .await
        .unwrap()
        .project
}

async fn subscribe(client: &mut Client, after: u64) -> SubscriptionId {
    client
        .call::<EventsSubscribe>(EventsSubscribeParams {
            after,
            project: None,
        })
        .await
        .unwrap()
        .subscription
}

fn created(event: &EventsEventParams) -> &Project {
    match &event.event {
        WispEvent::ProjectCreated { project } => project,
        other => panic!("expected project.created, got {other:?}"),
    }
}

#[tokio::test]
async fn subscribers_get_the_replay_then_live_events_on_every_connection() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut creator = Client::ready(&wispd.socket).await;
    let first = create(&mut creator, dir.path(), "wisp").await;

    let mut replaying = Client::ready(&wispd.socket).await;
    let replay = subscribe(&mut replaying, 0).await;
    let replayed = replaying.next_event().await;
    assert_eq!(replayed.subscription, replay);
    assert_eq!(replayed.seq, 1);
    assert_eq!(replayed.project, None, "project.created is host-level");
    assert_eq!(replayed.time, first.created_at);
    assert_eq!(created(&replayed), &first);

    let mut live = Client::ready(&wispd.socket).await;
    let snapshot = live
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap();
    let live_subscription = subscribe(&mut live, snapshot.seq).await;

    let second = create(&mut creator, dir.path(), "roster").await;
    for (client, subscription) in [(&mut replaying, replay), (&mut live, live_subscription)] {
        let delivered = event(client).await;
        assert_eq!(delivered.subscription, subscription);
        assert_eq!(delivered.seq, 2);
        assert_eq!(created(&delivered), &second);
    }
}

#[tokio::test]
async fn a_retried_create_adds_no_event() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    let params = create_params(dir.path(), "wisp");
    client.call::<ProjectCreate>(params.clone()).await.unwrap();
    client.call::<ProjectCreate>(params).await.unwrap();

    let mut watcher = Client::ready(&wispd.socket).await;
    subscribe(&mut watcher, 0).await;
    assert_eq!(watcher.next_event().await.seq, 1);
    watcher.stays_quiet(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn a_subscription_to_a_missing_project_is_refused() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    let project = create(&mut client, dir.path(), "wisp").await;

    let error = client
        .call::<EventsSubscribe>(EventsSubscribeParams {
            after: 0,
            project: Some(ProjectId::generate()),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::ProjectNotFound);

    // An existing project's subscription works. M1 has no project-level events yet, so it
    // stays quiet even though a host-level event exists.
    client
        .call::<EventsSubscribe>(EventsSubscribeParams {
            after: 0,
            project: Some(project.id),
        })
        .await
        .unwrap();
    client.stays_quiet(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn a_seq_this_log_never_reached_needs_a_resync() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;

    let error = client
        .call::<EventsSubscribe>(EventsSubscribeParams {
            after: 5,
            project: None,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::ResyncRequired);
}

#[tokio::test]
async fn unsubscribing_stops_the_events() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    let subscription = subscribe(&mut client, 0).await;
    client
        .call::<EventsUnsubscribe>(EventsUnsubscribeParams { subscription })
        .await
        .unwrap();
    client
        .call::<EventsUnsubscribe>(EventsUnsubscribeParams { subscription })
        .await
        .expect("ending a subscription that doesn't exist succeeds");

    let mut creator = Client::ready(&wispd.socket).await;
    create(&mut creator, dir.path(), "wisp").await;
    // The next message is the health answer, so no event came before it.
    client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
    client.stays_quiet(Duration::from_millis(200)).await;
}

#[tokio::test]
async fn events_older_than_the_retention_need_a_resync() {
    let dir = temp_dir();
    let mut config = InProcess::config(dir.path());
    config.event_retention = 2;
    let server = InProcess::start(config);
    let mut client = Client::ready(&server.socket).await;
    for name in ["a", "b", "c"] {
        create(&mut client, dir.path(), name).await;
    }

    let error = client
        .call::<EventsSubscribe>(EventsSubscribeParams {
            after: 0,
            project: None,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::ResyncRequired);

    subscribe(&mut client, 1).await;
    assert_eq!(client.next_event().await.seq, 2);
    assert_eq!(client.next_event().await.seq, 3);
    drop(client);
    server.stop().await;
}

#[tokio::test]
async fn events_reach_a_subscriber_while_it_is_also_making_requests() {
    let dir = temp_dir();
    let wispd = Wispd::start(dir.path()).await;
    let mut client = Client::ready(&wispd.socket).await;
    client
        .call::<EventsSubscribe>(EventsSubscribeParams {
            after: 0,
            project: None,
        })
        .await
        .unwrap();
    let create = client
        .send::<ProjectCreate>(create_params(dir.path(), "wisp"))
        .await;
    let mut saw_answer = false;
    let mut saw_event = false;
    while !(saw_answer && saw_event) {
        match client.next().await.expect("a message") {
            Message::Response(response) => {
                assert_eq!(response.id, Some(create.clone()));
                saw_answer = true;
            }
            Message::Notification(notification) => {
                assert_eq!(notification.method, "events/event");
                saw_event = true;
            }
            Message::Request(request) => panic!("unexpected request {request:?}"),
        }
    }
}
