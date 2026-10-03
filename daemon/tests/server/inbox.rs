//! A Project's inbox end to end (PLX-401, decision 0043): each source adds its item and appends
//! `inbox.added`, and `inbox/list` and `inbox/seen` read and mark them.

use parallax_protocol::methods::{AgentCancel, AgentStart, InboxList, InboxSeen, ProjectStart};
use parallax_protocol::{
    AccountChoice, AgentCancelParams, AgentStartParams, AgentStatus, CoordinatorThreadId,
    ErrorKind, InboxItem, InboxKind, InboxListParams, InboxSeenParams, ParallaxEvent, ProjectId,
    ProjectStartParams, RunId,
};
use plxd::backend::fake::{AskedApproval, Step};
use serde_json::json;

use crate::agents::{
    Conn, Host, create, end_turn, fake, init, project_params, start_params, subscribe, until,
    updated_to,
};
use crate::support::{kind, temp_dir};

/// A run `project`'s coordinator started, as its `spawn_agent` tool starts one.
fn child(project: ProjectId, task: &str) -> AgentStartParams {
    AgentStartParams {
        coordinator_thread: Some(CoordinatorThreadId::generate()),
        ..start_params(project, task)
    }
}

/// The next `inbox.added`, checked to be on `project`'s events.
async fn added(client: &mut Conn, project: ProjectId) -> InboxItem {
    let events = until(client, |event| {
        matches!(event.event, ParallaxEvent::InboxAdded { .. })
    })
    .await;
    let event = events.last().unwrap();
    assert_eq!(event.project, Some(project));
    let ParallaxEvent::InboxAdded { item } = &event.event else {
        unreachable!();
    };
    item.clone()
}

async fn list(client: &mut Conn, project: ProjectId) -> Vec<InboxItem> {
    client
        .call::<InboxList>(InboxListParams { project })
        .await
        .unwrap()
        .items
}

/// A child that finishes adds `done` with its diff stats, a run the client started itself adds
/// nothing, and `inbox/seen` marks the item once.
#[tokio::test]
async fn a_child_finishing_adds_done_with_its_diff_stats_and_seen_marks_it() {
    let host = Host::start(
        temp_dir(),
        fake(vec![
            init("session-1"),
            Step::WriteFile {
                path: "README.md".to_owned(),
                content: "# App\n".to_owned(),
            },
            end_turn("Done."),
        ]),
    );
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    subscribe(&mut client, project.id, 0).await;
    client
        .call::<AgentStart>(start_params(project.id, "Not a child."))
        .await
        .unwrap();
    until(&mut client, updated_to(AgentStatus::Completed)).await;

    let run = client
        .call::<AgentStart>(child(project.id, "Rewrite the README.\nKeep it short."))
        .await
        .unwrap()
        .run
        .id;
    let item = added(&mut client, project.id).await;
    assert_eq!(item.kind, InboxKind::Done);
    assert_eq!(item.run, run);
    assert_eq!(item.text, "Rewrite the README.: done, 1 files (+1 -1)");
    assert_eq!(item.seen_at, None);
    assert_eq!(
        list(&mut client, project.id).await,
        std::slice::from_ref(&item)
    );

    let seen = client
        .call::<InboxSeen>(InboxSeenParams {
            project: project.id,
            items: vec![item.id],
        })
        .await
        .unwrap()
        .items;
    let seen_at = seen[0].seen_at.expect("marked seen");
    let again = client
        .call::<InboxSeen>(InboxSeenParams {
            project: project.id,
            items: vec![item.id],
        })
        .await
        .unwrap()
        .items;
    assert_eq!(again[0].seen_at, Some(seen_at), "the first time is kept");
    assert_eq!(list(&mut client, project.id).await, seen);

    let unknown = client
        .call::<InboxList>(InboxListParams {
            project: ProjectId::generate(),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&unknown), ErrorKind::ProjectNotFound);
    host.server.stop().await;
}

/// A child whose CLI fails adds `failed`.
#[tokio::test]
async fn a_child_failing_adds_failed() {
    let host = Host::start(temp_dir(), fake(vec![init("session-1"), Step::Exit(3)]));
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    subscribe(&mut client, project.id, 0).await;
    let run = client
        .call::<AgentStart>(child(project.id, "Break it."))
        .await
        .unwrap()
        .run
        .id;
    let item = added(&mut client, project.id).await;
    assert_eq!(item.kind, InboxKind::Failed);
    assert_eq!(item.run, run);
    assert!(
        item.text.starts_with("Break it.: failed: "),
        "{}",
        item.text
    );
    host.server.stop().await;
}

/// A child's permission request (0031) adds `needsYou` while it waits.
#[tokio::test]
async fn a_childs_permission_request_adds_needs_you() {
    let host = Host::start(
        temp_dir(),
        fake(vec![
            init("session-1"),
            Step::RequestApproval(AskedApproval {
                tool_name: "Bash".to_owned(),
                input: json!({"command": "pnpm test"}),
                call_id: None,
                reason: None,
                always_allow: Vec::new(),
                interactive: false,
            }),
            Step::AwaitApproval,
        ]),
    );
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    subscribe(&mut client, project.id, 0).await;
    let run = client
        .call::<AgentStart>(AgentStartParams {
            approvals: true,
            ..child(project.id, "Run the tests.")
        })
        .await
        .unwrap()
        .run
        .id;
    let item = added(&mut client, project.id).await;
    assert_eq!(item.kind, InboxKind::NeedsYou);
    assert_eq!(item.run, run);
    assert_eq!(
        item.text,
        "Run the tests.: waiting for permission to use Bash"
    );
    host.server.stop().await;
}

/// The coordinator's wake-ups pausing (0025), here because the user stopped it, adds `needsYou`
/// about the coordinator.
#[tokio::test]
async fn paused_wake_ups_add_needs_you_about_the_coordinator() {
    let host = Host::start(
        temp_dir(),
        fake(vec![init("coordinator-1"), end_turn("Planned.")]),
    );
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    subscribe(&mut client, project.id, 0).await;
    let coordinator = client
        .call::<ProjectStart>(ProjectStartParams {
            project: project.id,
            run_id: RunId::generate(),
            prompt: "Plan.".to_owned(),
            account: Some(AccountChoice::Subscription {
                backend: "fake".to_owned(),
            }),
            model: None,
            effort: None,
            permission: None,
            images: Vec::new(),
            approvals: false,
        })
        .await
        .unwrap()
        .run
        .id;
    until(&mut client, updated_to(AgentStatus::Completed)).await;
    client
        .call::<AgentCancel>(AgentCancelParams {
            run_id: coordinator,
        })
        .await
        .unwrap();
    let item = added(&mut client, project.id).await;
    assert_eq!(item.kind, InboxKind::NeedsYou);
    assert_eq!(item.run, coordinator);
    assert!(item.text.starts_with("Wake-ups paused."), "{}", item.text);
    host.server.stop().await;
}
