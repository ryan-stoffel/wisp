//! A project's coordinator chat end to end (RYA-41, decision 0024): `project/start` against an
//! in-process plxd whose backend is the fake CLI, in a real git repository. The coordinator runs
//! in the project's repository, in the project's permission mode (0042).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::methods::{
    AgentCancel, AgentEvents, AgentList, AgentSend, AgentStart, EventsSubscribe, HostHealth,
    ProjectDelete, ProjectList, ProjectStart, ProjectUpdate, RepoAdd,
};
use parallax_protocol::{
    AccountChoice, AgentCancelParams, AgentEventsParams, AgentListParams, AgentOutputItem,
    AgentPermission, AgentPolicy, AgentRun, AgentSendParams, AgentStartParams, AgentStatus,
    CoordinatorThreadId, ErrorKind, EventsEventParams, EventsSubscribeParams, HostHealthParams,
    ParallaxEvent, ProjectCreateParams, ProjectDeleteParams, ProjectDeleteResult, ProjectId,
    ProjectListParams, ProjectPermission, ProjectStartParams, ProjectUpdateParams, Provider,
    RepoAddParams, RepoId, RunId, TurnId,
};
use plxd::backend::fake::{FakeBackend, Step};
use plxd::backend::{Backend, Capabilities, RunRequest, StartError, Started, ToolPolicy};
use plxd::paths::DataDir;
use plxd::routing::BackendRegistry;
use uuid::Uuid;

use crate::agents::{
    Conn, Host, create, end_turn, fake, fake_backend, git, init, items, project_params, real_repo,
    send_params, subscribe, text, until, updated_to,
};
use crate::support::{PATIENCE, kind, temp_dir};

/// The fake backend, keeping every request it is asked to start.
struct Recording {
    fake: FakeBackend,
    seen: Arc<Mutex<Vec<RunRequest>>>,
}

impl Backend for Recording {
    fn name(&self) -> &'static str {
        self.fake.name()
    }

    fn capabilities(&self) -> Capabilities {
        self.fake.capabilities()
    }

    fn permissions(&self) -> &'static [AgentPermission] {
        self.fake.permissions()
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        self.seen.lock().unwrap().push(request.clone());
        self.fake.start(request)
    }
}

fn recording(steps: Vec<Step>, seen: &Arc<Mutex<Vec<RunRequest>>>) -> BackendRegistry {
    let mut backends = BackendRegistry::new();
    backends.register(
        Provider::Anthropic,
        Arc::new(Recording {
            fake: fake_backend(steps),
            seen: Arc::clone(seen),
        }),
    );
    backends
}

fn start_params(project: ProjectId, prompt: &str) -> ProjectStartParams {
    ProjectStartParams {
        project,
        run_id: RunId::generate(),
        prompt: prompt.to_owned(),
        account: Some(AccountChoice::Subscription {
            backend: "fake".to_owned(),
        }),
        model: None,
        effort: None,
        permission: None,
        images: Vec::new(),
        approvals: false,
    }
}

#[tokio::test]
async fn a_coordinator_runs_in_the_projects_repository_and_resumes_there_after_a_restart() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let script = || {
        vec![
            init("coordinator-1"),
            text("Planning."),
            end_turn("Planned."),
        ]
    };
    let host = Host::start(temp_dir(), recording(script(), &seen));
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    let repo = PathBuf::from(&project.repo_path);
    let before = git(&repo, &["rev-parse", "HEAD"]);
    // The user's own uncommitted work stays in the checkout.
    std::fs::write(repo.join("notes.txt"), "mine\n").unwrap();
    subscribe(&mut client, project.id, 0).await;

    let params = start_params(project.id, "Plan the README.");
    let run = client
        .call::<ProjectStart>(params.clone())
        .await
        .unwrap()
        .run;
    assert_eq!(run.policy, AgentPolicy::NoWrite);
    let thread = CoordinatorThreadId::try_from(Uuid::from(run.id)).unwrap();
    assert_eq!(
        run.coordinator_thread,
        Some(thread),
        "its thread is its own id"
    );
    assert_eq!(run.worktree_path, None);
    let retried = client.call::<ProjectStart>(params.clone()).await.unwrap();
    assert_eq!(retried.run.id, run.id, "a retry returns the same run");
    let projects = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap()
        .projects;
    assert_eq!(projects[0].coordinator, Some(run.id));

    let events = until(&mut client, updated_to(AgentStatus::Completed)).await;
    assert!(items(&events).contains(&AgentOutputItem::Text {
        message_id: None,
        text: "Planning.".to_owned(),
    }));
    let first = seen.lock().unwrap()[0].clone();
    assert_eq!(first.policy, ToolPolicy::NoWrite);
    assert_eq!(first.cwd, repo, "it runs in the user's checkout");
    assert!(first.sandbox.is_none());
    let tools = first.coordinator_tools.expect("plxd's tools are attached");
    assert_eq!((tools.project, tools.thread), (project.id, thread));
    assert!(first.prompt.contains("spawn_agent"), "{}", first.prompt);
    assert!(
        first.prompt.ends_with("Plan the README."),
        "{}",
        first.prompt
    );
    assert_eq!(
        git(&repo, &["rev-parse", "HEAD"]),
        before,
        "a coordinator is never committed"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("notes.txt")).unwrap(),
        "mine\n"
    );

    let host = host.restart(recording(script(), &seen)).await;
    let mut client = host.client().await;
    let transcript = client
        .call::<AgentEvents>(AgentEventsParams {
            run_id: run.id,
            after: 0,
            limit: None,
        })
        .await
        .unwrap();
    assert!(
        transcript.events.iter().any(|logged| matches!(
            &logged.event,
            ParallaxEvent::AgentOutput { items, .. } if items.iter().any(|item| matches!(
                item, AgentOutputItem::Text { text, .. } if text == "Planning."
            ))
        )),
        "the transcript survives the restart"
    );
    subscribe(&mut client, project.id, 0).await;
    client
        .call::<AgentSend>(send_params(run.id, TurnId::generate(), "Go on."))
        .await
        .unwrap();
    until(&mut client, updated_to(AgentStatus::Completed)).await;
    let resumed = seen.lock().unwrap()[1].clone();
    assert_eq!(
        resumed.resume.map(|resume| resume.session_id).as_deref(),
        Some("coordinator-1"),
        "a message resumes the session"
    );
    assert_eq!(resumed.prompt, "Go on.");
    assert_eq!(resumed.cwd, repo, "the session resumes where it started");
    assert_eq!(resumed.policy, ToolPolicy::NoWrite);
    assert!(resumed.coordinator_tools.is_some());
    host.server.stop().await;
}

#[tokio::test]
async fn a_new_start_replaces_the_coordinator_only_once_it_stops_running() {
    let script = vec![
        init("coordinator-1"),
        Step::AwaitFollowUp,
        end_turn("Done."),
    ];
    let host = Host::start(temp_dir(), fake(script));
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    subscribe(&mut client, project.id, 0).await;

    let first = client
        .call::<ProjectStart>(start_params(project.id, "Plan."))
        .await
        .unwrap()
        .run;
    let refused = client
        .call::<ProjectStart>(start_params(project.id, "Start over."))
        .await
        .unwrap_err();
    assert_eq!(
        kind(&refused),
        ErrorKind::IdConflict,
        "one live coordinator"
    );

    client
        .call::<AgentSend>(send_params(first.id, TurnId::generate(), "Wrap up."))
        .await
        .unwrap();
    until(&mut client, updated_to(AgentStatus::Completed)).await;
    // Once it stops, even with a session that might never resume, a new start replaces it.
    let second = client
        .call::<ProjectStart>(start_params(project.id, "Start over."))
        .await
        .unwrap()
        .run;
    let projects = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap()
        .projects;
    assert_eq!(projects[0].coordinator, Some(second.id));
    // The replaced one stays stopped, so the project's worktree has one coordinator.
    let refused = client
        .call::<AgentSend>(send_params(first.id, TurnId::generate(), "Still there?"))
        .await
        .unwrap_err();
    assert_eq!(kind(&refused), ErrorKind::RunNotResumable);
    host.server.stop().await;
}

/// Workers on one script, and each coordinator launch on the next of its own, keeping every
/// request.
struct Roles {
    worker: FakeBackend,
    coordinator: Mutex<Vec<FakeBackend>>,
    seen: Arc<Mutex<Vec<RunRequest>>>,
    permissions: &'static [AgentPermission],
}

impl Backend for Roles {
    fn name(&self) -> &'static str {
        self.worker.name()
    }

    fn capabilities(&self) -> Capabilities {
        self.worker.capabilities()
    }

    fn permissions(&self) -> &'static [AgentPermission] {
        self.permissions
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        self.seen.lock().unwrap().push(request.clone());
        if request.policy == ToolPolicy::NoWrite {
            self.coordinator.lock().unwrap().remove(0).start(request)
        } else {
            self.worker.start(request)
        }
    }
}

/// Workers on `worker`, and each coordinator launch on the next of `coordinator`, in Auto or
/// Bypass.
fn roles(
    worker: Vec<Step>,
    coordinator: Vec<Vec<Step>>,
    seen: &Arc<Mutex<Vec<RunRequest>>>,
) -> BackendRegistry {
    roles_mapping(
        worker,
        coordinator,
        seen,
        &[AgentPermission::Auto, AgentPermission::Bypass],
    )
}

/// [`roles`], mapping only `permissions`.
fn roles_mapping(
    worker: Vec<Step>,
    coordinator: Vec<Vec<Step>>,
    seen: &Arc<Mutex<Vec<RunRequest>>>,
    permissions: &'static [AgentPermission],
) -> BackendRegistry {
    let mut backends = BackendRegistry::new();
    backends.register(
        Provider::Anthropic,
        Arc::new(Roles {
            worker: fake_backend(worker),
            coordinator: Mutex::new(coordinator.into_iter().map(fake_backend).collect()),
            seen: Arc::clone(seen),
            permissions,
        }),
    );
    backends
}

/// Every coordinator launch `seen` so far.
fn coordinator_launches(seen: &Mutex<Vec<RunRequest>>) -> Vec<RunRequest> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|request| request.policy == ToolPolicy::NoWrite)
        .cloned()
        .collect()
}

/// The `n`th coordinator launch, once it happens.
async fn nth_launch(seen: &Mutex<Vec<RunRequest>>, n: usize) -> RunRequest {
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Some(launch) = coordinator_launches(seen).get(n) {
            return launch.clone();
        }
        assert!(Instant::now() < deadline, "the coordinator was never woken");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A worker `project`'s coordinator starts through its tools, as `spawn_agent` would.
async fn spawn(client: &mut Conn, coordinator: &AgentRun, task: &str) -> RunId {
    let params = AgentStartParams {
        coordinator_thread: coordinator.coordinator_thread,
        ..crate::agents::start_params(coordinator.project, task)
    };
    client.call::<AgentStart>(params).await.unwrap().run.id
}

/// Waits until each of `runs` has reported its session, so it can be resumed.
async fn sessions(client: &mut Conn, runs: &[RunId]) {
    let mut left = runs.to_vec();
    until(client, |event| {
        if let ParallaxEvent::AgentUpdated { run_id, state } = &event.event
            && state.session_id.is_some()
        {
            left.retain(|run| run != run_id);
        }
        left.is_empty()
    })
    .await;
}

/// PLX-394 (0042): the coordinator and the runs it spawns run in the Project's mode, whatever
/// they ask for, and a mode `project/update` changes applies from each run's next CLI process.
#[tokio::test]
async fn a_projects_runs_run_in_its_mode_and_a_new_mode_applies_from_their_next_process() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    // Workers never finish, so no wake-up takes a coordinator script.
    let backends = roles(
        vec![init("worker-1"), Step::AwaitFollowUp],
        vec![
            vec![init("coordinator-1"), end_turn("Planned.")],
            vec![init("coordinator-1"), end_turn("Planned again.")],
        ],
        &seen,
    );
    let host = Host::start(temp_dir(), backends);
    let mut client = host.client().await;
    let project = create(
        &mut client,
        ProjectCreateParams {
            permission: Some(ProjectPermission::Bypass),
            ..project_params(host.dir.path())
        },
    )
    .await;
    assert_eq!(project.permission, Some(ProjectPermission::Bypass));
    subscribe(&mut client, project.id, 0).await;
    let coordinator = client
        .call::<ProjectStart>(ProjectStartParams {
            permission: Some(AgentPermission::Plan),
            ..start_params(project.id, "Plan.")
        })
        .await
        .unwrap()
        .run;
    assert_eq!(coordinator.permission, Some(AgentPermission::Bypass));
    until(&mut client, updated_to(AgentStatus::Completed)).await;
    assert_eq!(
        nth_launch(&seen, 0).await.permission,
        Some(AgentPermission::Bypass)
    );
    let asked = client
        .call::<AgentStart>(AgentStartParams {
            coordinator_thread: coordinator.coordinator_thread,
            permission: Some(AgentPermission::Edit),
            ..crate::agents::start_params(project.id, "Fix a typo.")
        })
        .await
        .unwrap()
        .run
        .id;

    let updated = client
        .call::<ProjectUpdate>(ProjectUpdateParams {
            project: project.id,
            name: None,
            icon: None,
            permission: Some(ProjectPermission::Auto),
        })
        .await
        .unwrap()
        .project;
    assert_eq!(updated.permission, Some(ProjectPermission::Auto));
    client
        .call::<AgentSend>(AgentSendParams {
            permission: Some(AgentPermission::Plan),
            ..send_params(coordinator.id, TurnId::generate(), "Plan more.")
        })
        .await
        .unwrap();
    assert_eq!(
        nth_launch(&seen, 1).await.permission,
        Some(AgentPermission::Auto)
    );
    let after = spawn(&mut client, &coordinator, "Add a license.").await;

    let launched = |run: RunId| {
        seen.lock()
            .unwrap()
            .iter()
            .find(|request| request.run_id == run)
            .map(|request| request.permission)
    };
    assert_eq!(launched(asked), Some(Some(AgentPermission::Bypass)));
    assert_eq!(launched(after), Some(Some(AgentPermission::Auto)));
    host.server.stop().await;
}

/// PLX-394 (0042): a backend without the Project's mode is refused with why, and never moved up
/// to Bypass. A Project created without a mode, as by an older app, is in Auto.
#[tokio::test]
async fn a_backend_without_the_projects_mode_is_refused_and_never_moved_up() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let backends = roles_mapping(
        vec![init("worker-1"), Step::AwaitFollowUp],
        vec![vec![init("coordinator-1"), end_turn("Planned.")]],
        &seen,
        &[AgentPermission::Edit, AgentPermission::Bypass],
    );
    let host = Host::start(temp_dir(), backends);
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    assert_eq!(project.permission, Some(ProjectPermission::Auto));

    let error = client
        .call::<ProjectStart>(start_params(project.id, "Plan."))
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::UnsupportedOption);
    assert_eq!(
        error.message,
        "fake has no Auto. Set the Project to Bypass to use it."
    );
    let worker = client
        .call::<AgentStart>(crate::agents::start_params(project.id, "Fix a typo."))
        .await
        .unwrap_err();
    assert_eq!(worker.message, error.message);
    assert!(seen.lock().unwrap().is_empty(), "nothing started");

    client
        .call::<ProjectUpdate>(ProjectUpdateParams {
            project: project.id,
            name: None,
            icon: None,
            permission: Some(ProjectPermission::Bypass),
        })
        .await
        .unwrap();
    let coordinator = client
        .call::<ProjectStart>(start_params(project.id, "Plan."))
        .await
        .unwrap()
        .run;
    assert_eq!(coordinator.permission, Some(AgentPermission::Bypass));
    host.server.stop().await;
}

/// RYA-222 (0031): a coordinator whose client answers permission requests keeps `approvals` when
/// it resumes, and the subagents it spawns get them too. One started without them, as an older
/// app starts it, and its subagents never ask.
#[tokio::test]
async fn approvals_last_through_a_resume_and_reach_the_coordinators_subagents() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    // Workers never finish, so no wake-up takes a coordinator script.
    let backends = roles(
        vec![init("worker-1"), Step::AwaitFollowUp],
        vec![
            vec![init("coordinator-1"), end_turn("Planned.")],
            vec![init("coordinator-1"), end_turn("Planned again.")],
            vec![init("coordinator-2"), end_turn("Planned anew.")],
        ],
        &seen,
    );
    let host = Host::start(temp_dir(), backends);
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    subscribe(&mut client, project.id, 0).await;
    let answering = client
        .call::<ProjectStart>(ProjectStartParams {
            approvals: true,
            ..start_params(project.id, "Plan.")
        })
        .await
        .unwrap()
        .run;
    until(&mut client, updated_to(AgentStatus::Completed)).await;
    let subagent = spawn(&mut client, &answering, "Add a README.").await;
    client
        .call::<AgentSend>(send_params(answering.id, TurnId::generate(), "Plan more."))
        .await
        .unwrap();
    assert!(nth_launch(&seen, 0).await.approvals);
    assert!(nth_launch(&seen, 1).await.approvals, "a resume keeps them");
    until(&mut client, updated_to(AgentStatus::Completed)).await;

    // Starting over, from a client that doesn't answer.
    let quiet = client
        .call::<ProjectStart>(start_params(project.id, "Start over."))
        .await
        .unwrap()
        .run;
    assert!(!nth_launch(&seen, 2).await.approvals);
    until(&mut client, updated_to(AgentStatus::Completed)).await;
    let quiet_subagent = spawn(&mut client, &quiet, "Add a license.").await;

    let launched = |run: RunId| {
        seen.lock()
            .unwrap()
            .iter()
            .find(|request| request.run_id == run)
            .map(|request| request.approvals)
    };
    assert_eq!(launched(subagent), Some(true));
    assert_eq!(launched(quiet_subagent), Some(false));
    host.server.stop().await;
}

/// RYA-42: two runs the coordinator started finish during its turn; once that turn ends, and with
/// no client connected, plxd wakes it with one turn that names both.
#[tokio::test]
async fn runs_finishing_during_a_coordinator_turn_wake_it_once_with_no_client_connected() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let backends = roles(
        vec![init("worker-1"), end_turn("Added it.")],
        vec![
            vec![
                init("coordinator-1"),
                Step::AwaitFollowUp,
                end_turn("Planned."),
            ],
            vec![init("coordinator-1"), end_turn("Reviewed.")],
        ],
        &seen,
    );
    let host = Host::start(temp_dir(), backends);
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    subscribe(&mut client, project.id, 0).await;
    let coordinator = client
        .call::<ProjectStart>(start_params(project.id, "Plan."))
        .await
        .unwrap()
        .run;

    let mut workers = Vec::new();
    for task in ["Add a README.", "Add a license."] {
        workers.push(spawn(&mut client, &coordinator, task).await);
    }
    let mut left = workers.len();
    until(&mut client, |event| {
        if matches!(&event.event, ParallaxEvent::AgentUpdated { run_id, state }
            if workers.contains(run_id) && state.status == AgentStatus::Completed)
        {
            left -= 1;
        }
        left == 0
    })
    .await;
    // Past wake-ups' 2 s batch: a turn in progress still holds them.
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(
        coordinator_launches(&seen).len(),
        1,
        "no wake-up during a turn"
    );

    // The user's message ends the coordinator's turn; then nobody is watching.
    client
        .call::<AgentSend>(send_params(coordinator.id, TurnId::generate(), "Go on."))
        .await
        .unwrap();
    drop(client);
    let wake = nth_launch(&seen, 1).await;
    assert!(wake.resume.is_some(), "a wake-up resumes the session");
    for worker in &workers {
        assert!(wake.prompt.contains(&worker.to_string()), "{}", wake.prompt);
    }
    assert!(
        wake.prompt.contains("completed, saying: Added it."),
        "{}",
        wake.prompt
    );

    let mut client = host.client().await;
    subscribe(&mut client, project.id, 0).await;
    let events = until(&mut client, |event| {
        matches!(&event.event, ParallaxEvent::AgentOutput { items, .. } if items.iter().any(|item|
            matches!(item, AgentOutputItem::TurnStarted { wake: true, .. })))
    })
    .await;
    let Some(ParallaxEvent::AgentOutput { run_id, items }) = events.last().map(|e| &e.event) else {
        unreachable!();
    };
    assert_eq!(*run_id, coordinator.id);
    assert!(items.contains(&AgentOutputItem::TurnStarted {
        turn_id: wake.turn_id,
        text: Some(wake.prompt.clone()),
        wake: true,
        images: Vec::new(),
        threads: Vec::new(),
    }));
    host.server.stop().await;
}

/// RYA-178: a run the coordinator started is running, and so is the coordinator's own turn, when
/// plxd restarts. Once it is back, one wake-up names both, and another restart wakes nothing.
#[tokio::test]
async fn a_restart_mid_run_wakes_the_coordinator_once_naming_what_it_interrupted() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hang = || vec![init("worker-1"), Step::Hang];
    let backends = roles(hang(), vec![vec![init("coordinator-1"), Step::Hang]], &seen);
    let host = Host::start(temp_dir(), backends);
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    subscribe(&mut client, project.id, 0).await;
    let coordinator = client
        .call::<ProjectStart>(start_params(project.id, "Plan."))
        .await
        .unwrap()
        .run;
    let worker = spawn(&mut client, &coordinator, "Add a README.").await;
    sessions(&mut client, &[coordinator.id, worker]).await;

    let later = || vec![vec![init("coordinator-1"), end_turn("Picked up.")]];
    let host = host.restart(roles(hang(), later(), &seen)).await;
    let wake = nth_launch(&seen, 1).await;
    assert_eq!(
        wake.resume.map(|resume| resume.session_id).as_deref(),
        Some("coordinator-1"),
        "a wake-up resumes the session"
    );
    assert!(
        wake.prompt.starts_with("Parallax, not the user"),
        "{}",
        wake.prompt
    );
    assert!(
        wake.prompt.contains(&format!(
            "- Run {worker} (Add a README.): interrupted when plxd stopped"
        )),
        "{}",
        wake.prompt
    );
    assert!(
        wake.prompt.contains("Your own last turn was interrupted"),
        "{}",
        wake.prompt
    );
    assert_eq!(wake.prompt.matches("- Run ").count(), 1, "{}", wake.prompt);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(coordinator_launches(&seen).len(), 2, "one wake-up");

    // The coordinator heard about the run and let it be: nothing new to wake it for.
    let host = host.restart(roles(hang(), later(), &seen)).await;
    host.client().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(coordinator_launches(&seen).len(), 2, "nothing new");
    host.server.stop().await;
    // The wake-up counts against the cap, and the count outlives plxd.
    let store =
        parallax_store::Store::open(DataDir::new(host.dir.path()).unwrap().store_file()).unwrap();
    assert_eq!(store.wake_state(coordinator.id.into()).unwrap().in_a_row, 1);
    // The coordinator's run is its worker's parent, and its own has none (0041).
    let parent = |id: RunId| store.get_run(id.into()).unwrap().unwrap().fields.parent;
    assert_eq!(parent(worker), Some(coordinator.id.into()));
    assert_eq!(parent(coordinator.id), None);
}

/// RYA-178: the user stops the coordinator while a run it started is running, then plxd
/// restarts. Wake-ups stay paused, and what the restart interrupted waits for the user's message.
#[tokio::test]
async fn a_pause_survives_a_restart_and_what_waits_follows_the_users_message() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hang = || vec![init("worker-1"), Step::Hang];
    let turn = |result: &str| vec![init("coordinator-1"), end_turn(result)];
    let host = Host::start(temp_dir(), roles(hang(), vec![turn("Planned.")], &seen));
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    subscribe(&mut client, project.id, 0).await;
    let coordinator = client
        .call::<ProjectStart>(start_params(project.id, "Plan."))
        .await
        .unwrap()
        .run;
    until(&mut client, updated_to(AgentStatus::Completed)).await;
    let worker = spawn(&mut client, &coordinator, "Add a README.").await;
    sessions(&mut client, &[worker]).await;
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

    let backends = roles(hang(), vec![turn("Heard you."), turn("Reviewed.")], &seen);
    let host = host.restart(backends).await;
    let mut client = host.client().await;
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(coordinator_launches(&seen).len(), 1, "still paused");

    client
        .call::<AgentSend>(send_params(coordinator.id, TurnId::generate(), "Go on."))
        .await
        .unwrap();
    let wake = nth_launch(&seen, 2).await;
    assert!(
        wake.prompt
            .contains(&format!("- Run {worker} (Add a README.): interrupted")),
        "{}",
        wake.prompt
    );
    host.server.stop().await;
}

fn subscribe_host(after: u64) -> EventsSubscribeParams {
    EventsSubscribeParams {
        after,
        project: None,
    }
}

async fn delete(client: &mut Conn, project: ProjectId) -> Result<ProjectDeleteResult, ErrorObject> {
    client
        .call::<ProjectDelete>(ProjectDeleteParams { project })
        .await
}

async fn runs_of(client: &mut Conn, project: ProjectId) -> Vec<AgentRun> {
    let params = AgentListParams {
        project: Some(project),
    };
    client.call::<AgentList>(params).await.unwrap().runs
}

fn deleted(project: ProjectId) -> impl FnMut(&EventsEventParams) -> bool {
    move |event| matches!(&event.event, ParallaxEvent::ProjectDeleted { project: id } if *id == project)
}

/// PLX-338: deleting a project stops its coordinator and the subagent it started, and removes
/// their runs, events, worktree, and branch, its context folder, and the project itself, which
/// `project.deleted` tells every client, then and on replay.
#[tokio::test]
async fn deleting_a_project_stops_its_agents_and_removes_everything() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let backends = roles(
        vec![init("worker-1"), text("Working"), Step::Hang],
        vec![vec![init("coordinator-1"), Step::Hang]],
        &seen,
    );
    let host = Host::start(temp_dir(), backends);
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    let repo = PathBuf::from(&project.repo_path);
    let context = host.dir.path().join("context").join(project.id.to_string());
    assert!(context.is_dir());
    client
        .call::<EventsSubscribe>(subscribe_host(0))
        .await
        .unwrap();
    subscribe(&mut client, project.id, 0).await;
    let coordinator = client
        .call::<ProjectStart>(start_params(project.id, "Plan."))
        .await
        .unwrap()
        .run;
    let worker = spawn(&mut client, &coordinator, "Build it.").await;
    let mut running = vec![coordinator.id, worker];
    until(&mut client, |event| {
        if let ParallaxEvent::AgentUpdated { run_id, state } = &event.event
            && state.status == AgentStatus::Running
        {
            running.retain(|run| run != run_id);
        }
        running.is_empty()
    })
    .await;
    let run = runs_of(&mut client, project.id)
        .await
        .into_iter()
        .find(|run| run.id == worker)
        .unwrap();
    let worktree = PathBuf::from(run.worktree_path.unwrap());
    let branch = run.branch.unwrap();
    assert!(worktree.is_dir());

    delete(&mut client, project.id).await.unwrap();
    until(&mut client, deleted(project.id)).await;
    let health = client
        .call::<HostHealth>(HostHealthParams {})
        .await
        .unwrap();
    assert_eq!(health.running_agents, 0, "both agents were stopped");
    let projects = client
        .call::<ProjectList>(ProjectListParams {})
        .await
        .unwrap()
        .projects;
    assert!(projects.is_empty());
    assert!(runs_of(&mut client, project.id).await.is_empty());
    for run_id in [coordinator.id, worker] {
        let events = client
            .call::<AgentEvents>(AgentEventsParams {
                run_id,
                after: 0,
                limit: None,
            })
            .await
            .unwrap_err();
        assert_eq!(kind(&events), ErrorKind::RunNotFound);
    }
    assert!(!worktree.exists(), "the worktree is removed");
    let branches = git(&repo, &["branch", "--list", &branch]);
    assert!(branches.is_empty(), "the branch is removed: {branches}");
    assert!(!context.exists(), "the project's notes are removed");
    assert!(repo.join("README.md").is_file(), "the repository stays");

    let mut replay = host.client().await;
    replay
        .call::<EventsSubscribe>(subscribe_host(0))
        .await
        .unwrap();
    until(&mut replay, deleted(project.id)).await;

    let again = delete(&mut client, project.id).await.unwrap_err();
    assert_eq!(kind(&again), ErrorKind::ProjectNotFound);
    host.server.stop().await;
}

/// PLX-338: only a project can be deleted. An unknown id and a repo entry's id, which its
/// threads' runs use as their project id, both fail with `projectNotFound`.
#[tokio::test]
async fn deleting_an_unknown_project_or_a_repo_entry_fails() {
    let host = Host::start(temp_dir(), fake(Vec::new()));
    let mut client = host.client().await;
    let work = temp_dir();
    let entry = client
        .call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: real_repo(work.path()).to_str().unwrap().to_owned(),
        })
        .await
        .unwrap()
        .repo;
    let scope = ProjectId::try_from(Uuid::from(entry.id)).unwrap();
    for project in [ProjectId::generate(), scope] {
        let error = delete(&mut client, project).await.unwrap_err();
        assert_eq!(kind(&error), ErrorKind::ProjectNotFound, "{project}");
    }
    let listed = client
        .call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: entry.path.clone(),
        })
        .await
        .unwrap()
        .repo;
    assert_eq!(listed.id, entry.id, "the repo entry stays");
    host.server.stop().await;
}
