//! A Project's inbox end to end (PLX-401, decision 0043): each source adds its item and appends
//! `inbox.added`, and `inbox/list` and `inbox/seen` read and mark them.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use parallax_protocol::methods::{
    AgentCancel, AgentSend, AgentStart, InboxList, InboxSeen, ProjectStart,
};
use parallax_protocol::{
    AccountChoice, AgentCancelParams, AgentStartParams, AgentStatus, CoordinatorThreadId,
    ErrorKind, InboxItem, InboxKind, InboxListParams, InboxSeenParams, ParallaxEvent, ProjectId,
    ProjectStartParams, Provider, RunId, TurnId,
};
use plxd::backend::fake::{AskedApproval, FakeBackend, Step};
use plxd::backend::{Backend, Capabilities, RunRequest, StartError, Started};
use plxd::routing::BackendRegistry;
use serde_json::json;

use crate::agents::{
    Conn, Host, create, end_turn, fake, fake_backend, init, project_params, send_params,
    start_params, subscribe, until, updated_to,
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

/// The fake CLI for its first `starts` launches, then a backend that can't start one, as when a
/// wake-up's resume fails.
struct FailingAfter {
    fake: FakeBackend,
    starts: AtomicUsize,
}

impl Backend for FailingAfter {
    fn name(&self) -> &'static str {
        self.fake.name()
    }

    fn capabilities(&self) -> Capabilities {
        self.fake.capabilities()
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        if self.starts.fetch_sub(1, Ordering::SeqCst) == 0 {
            self.starts.store(0, Ordering::SeqCst);
            return Err(StartError::Unsupported("refused".to_owned()));
        }
        self.fake.start(request)
    }
}

/// The user's Stop pauses the coordinator's wake-ups (0025) and adds nothing. A wake-up that
/// can't resume it pauses them too, and that adds `needsYou` about the coordinator.
#[tokio::test]
async fn a_failed_wake_up_adds_needs_you_and_stop_adds_nothing() {
    let mut backends = BackendRegistry::new();
    backends.register(
        Provider::Anthropic,
        Arc::new(FailingAfter {
            fake: fake_backend(vec![init("session-1"), end_turn("Done.")]),
            // The coordinator's turn, the user's message after Stop, and the child.
            starts: AtomicUsize::new(3),
        }),
    );
    let host = Host::start(temp_dir(), backends);
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
        .run;
    until(&mut client, updated_to(AgentStatus::Completed)).await;
    client
        .call::<AgentCancel>(AgentCancelParams {
            run_id: coordinator.id,
        })
        .await
        .unwrap();
    until(&mut client, |event| {
        matches!(event.event, ParallaxEvent::AgentWakeupsPaused { .. })
    })
    .await;
    // The user's message lets wake-ups through again.
    client
        .call::<AgentSend>(send_params(coordinator.id, TurnId::generate(), "Go on."))
        .await
        .unwrap();
    until(&mut client, updated_to(AgentStatus::Completed)).await;

    let child = client
        .call::<AgentStart>(AgentStartParams {
            coordinator_thread: coordinator.coordinator_thread,
            ..start_params(project.id, "Add a README.")
        })
        .await
        .unwrap()
        .run
        .id;
    let done = added(&mut client, project.id).await;
    assert_eq!((done.kind, done.run), (InboxKind::Done, child));
    let paused = added(&mut client, project.id).await;
    assert_eq!(paused.kind, InboxKind::NeedsYou);
    assert_eq!(paused.run, coordinator.id);
    assert!(
        paused.text.starts_with("Wake-ups paused."),
        "{}",
        paused.text
    );
    assert_eq!(
        list(&mut client, project.id).await,
        [done, paused],
        "Stop added nothing"
    );
    host.server.stop().await;
}
