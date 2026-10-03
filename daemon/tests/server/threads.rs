//! Normal threads end to end (#110): `thread/*` and `repo/*` against an in-process server whose
//! worker backend is the fake CLI, in real git repositories.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use parallax_protocol::jsonrpc::{ErrorObject, INVALID_PARAMS, Message, Notification, RequestId};
use parallax_protocol::methods::{
    AgentAccept, AgentCancel, AgentEvents, AgentList, AgentSend, EventsEvent, EventsSubscribe,
    HostHealth, NotificationMethod, RepoAdd, RepoRefs, RepoUpdate, RequestMethod, ThreadArchive,
    ThreadDelete, ThreadList, ThreadStart, ThreadUpdate,
};
use parallax_protocol::{
    AcceptId, AccountChoice, AgentAcceptParams, AgentCancelParams, AgentEffort, AgentEventsParams,
    AgentListParams, AgentPermission, AgentSendParams, AgentStatus, ErrorKind, EventsEventParams,
    EventsSubscribeParams, HostHealthParams, ImageMediaType, ParallaxEvent, ProjectIcon, ProjectId,
    PromptImage, Provider, Repo, RepoAddParams, RepoId, RepoRefsParams, RepoUpdateParams, RunId,
    ThreadArchiveParams, ThreadDeleteParams, ThreadListParams, ThreadListResult, ThreadStartParams,
    ThreadUpdateParams, TurnId,
};
use plxd::backend::fake::{FakeBackend, Script, Step};
use plxd::backend::process::{CancelPolicy, Environment, Launcher};
use plxd::backend::{Backend, Capabilities, Event, RunRequest, StartError, Started};
use plxd::paths::DataDir;
use plxd::routing::BackendRegistry;
use rustix::process::Signal;
use tempfile::TempDir;
use tokio::time::Instant;

use crate::support::{Client, InProcess, PATIENCE, kind, temp_dir};

mod context;
mod files;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .expect("git runs");
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

fn real_repo(dir: &Path, name: &str) -> PathBuf {
    let repo = dir.join("repos").join(name);
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "--initial-branch=main"]);
    git(&repo, &["config", "user.name", "Test User"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    std::fs::write(repo.join("README.md"), "hello\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    repo.canonicalize().unwrap()
}

fn fake_backend(steps: Vec<Step>) -> FakeBackend {
    let scratch = tempfile::tempdir().unwrap();
    let launcher = Launcher::new(
        DataDir::new(scratch.path()).unwrap(),
        Environment::inherited(),
    );
    FakeBackend::new(launcher, Script { steps }).with_cancel_policy(CancelPolicy {
        signal: Signal::INT,
        group: false,
        grace: Duration::from_millis(500),
    })
}

fn fake(steps: Vec<Step>) -> BackendRegistry {
    let mut backends = BackendRegistry::new();
    backends.register(Provider::Anthropic, Arc::new(fake_backend(steps)));
    backends
}

/// A run's model, effort, permission, approvals, and whether it ran as a thread (0034), as its
/// backend got them.
type Options = (
    Option<String>,
    Option<AgentEffort>,
    Option<AgentPermission>,
    bool,
    bool,
);

/// The fake CLI as a backend that maps only `low` and `high` effort and the `plan` permission
/// (RYA-97), and records each run's options.
struct WithOptions {
    fake: FakeBackend,
    seen: Arc<Mutex<Vec<Options>>>,
}

impl Backend for WithOptions {
    fn name(&self) -> &'static str {
        self.fake.name()
    }

    fn capabilities(&self) -> Capabilities {
        self.fake.capabilities()
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        let options = (
            request.model.clone(),
            request.effort,
            request.permission,
            request.approvals,
            request.thread,
        );
        self.seen.lock().unwrap().push(options);
        self.fake.start(request)
    }

    fn efforts(&self) -> &'static [AgentEffort] {
        &[AgentEffort::Low, AgentEffort::High]
    }

    fn permissions(&self) -> &'static [AgentPermission] {
        &[AgentPermission::Plan]
    }
}

/// A host whose worker backend is [`WithOptions`] running `steps`, and what that backend saw.
fn with_options(steps: Vec<Step>) -> (Host, Arc<Mutex<Vec<Options>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut backends = BackendRegistry::new();
    backends.register(
        Provider::Anthropic,
        Arc::new(WithOptions {
            fake: fake_backend(steps),
            seen: Arc::clone(&seen),
        }),
    );
    (Host::start(backends), seen)
}

fn editing() -> Vec<Step> {
    vec![
        Step::Init {
            session_id: "thread-1".to_owned(),
            model: None,
        },
        Step::WriteFile {
            path: "NOTES.md".to_owned(),
            content: "Written in a thread.\n".to_owned(),
        },
        Step::Emit(Event::Text {
            message_id: None,
            text: "Done.".to_owned(),
        }),
        Step::EndTurn {
            result: Some("Done.".to_owned()),
        },
    ]
}

fn hang() -> Vec<Step> {
    vec![
        Step::Init {
            session_id: "hang-1".to_owned(),
            model: None,
        },
        Step::Hang,
    ]
}

/// `agent/send` params for `text`, changing no run options.
fn message(run_id: RunId, text: &str) -> AgentSendParams {
    AgentSendParams {
        run_id,
        turn_id: TurnId::generate(),
        text: text.to_owned(),
        model: None,
        effort: None,
        permission: None,
        context_window: None,
        fast: None,
        account: None,
        images: Vec::new(),
        threads: Vec::new(),
        from: None,
    }
}

/// `thread/update` params that mark `run_id` seen or snooze it (0033), and change nothing else.
fn attention(
    run_id: RunId,
    seen: bool,
    snoozed_until: Option<jiff::Timestamp>,
) -> ThreadUpdateParams {
    ThreadUpdateParams {
        run_id,
        seen,
        snoozed_until,
        title: None,
        settled: None,
    }
}

fn start_params(repo: Option<RepoId>, prompt: &str) -> ThreadStartParams {
    ThreadStartParams {
        run_id: RunId::generate(),
        repo,
        parent: None,
        title: None,
        prompt: prompt.to_owned(),
        account: Some(AccountChoice::Subscription {
            backend: "fake".to_owned(),
        }),
        model: None,
        effort: None,
        permission: None,
        context_window: None,
        fast: None,
        branch_slug: None,
        images: Vec::new(),
        approvals: false,
        checkout: false,
        base: None,
        checkout_ref: None,
        threads: Vec::new(),
    }
}

struct Host {
    /// plxd's data folder.
    dir: TempDir,
    /// Where the tests' own repositories live: outside the data folder, which `repo/add` refuses.
    work: TempDir,
    server: InProcess,
}

impl Host {
    fn start(backends: BackendRegistry) -> Self {
        let dir = temp_dir();
        let mut config = InProcess::config(dir.path());
        config.backends = Some(backends);
        let server = InProcess::start(config);
        Self {
            dir,
            work: temp_dir(),
            server,
        }
    }

    async fn client(&self) -> Conn {
        let mut client = Client::connect(&self.server.socket).await;
        client.initialize().await.expect("initialize");
        Conn {
            client,
            pending: VecDeque::new(),
        }
    }

    fn data(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    /// Stops plxd and starts a new one on the same data folder.
    async fn restart(self, backends: BackendRegistry) -> Self {
        let Self { dir, work, server } = self;
        server.stop().await;
        let mut config = InProcess::config(dir.path());
        config.backends = Some(backends);
        Self {
            dir,
            work,
            server: InProcess::start(config),
        }
    }
}

struct Conn {
    client: Client,
    pending: VecDeque<EventsEventParams>,
}

fn event(notification: Notification) -> EventsEventParams {
    assert_eq!(
        notification.method,
        <EventsEvent as NotificationMethod>::NAME
    );
    serde_json::from_value(notification.params.expect("params")).expect("an event")
}

impl Conn {
    async fn call<M: RequestMethod>(
        &mut self,
        params: M::Params,
    ) -> Result<M::Result, ErrorObject> {
        let id = self.client.send::<M>(params).await;
        loop {
            match self.client.next().await {
                Some(Message::Response(response)) => {
                    assert_eq!(response.id, Some(id));
                    return response.into_result();
                }
                Some(Message::Notification(notification)) => {
                    self.pending.push_back(event(notification));
                }
                other => panic!("expected a response, got {other:?}"),
            }
        }
    }

    async fn subscribe(&mut self, after: u64, project: Option<ProjectId>) {
        self.call::<EventsSubscribe>(EventsSubscribeParams { after, project })
            .await
            .unwrap();
    }

    async fn until(
        &mut self,
        mut done: impl FnMut(&EventsEventParams) -> bool,
    ) -> Vec<EventsEventParams> {
        let deadline = Instant::now() + PATIENCE;
        let mut events = Vec::new();
        loop {
            assert!(
                Instant::now() < deadline,
                "gave up waiting; got {events:#?}"
            );
            let event = match self.pending.pop_front() {
                Some(event) => event,
                None => match self.client.next().await {
                    Some(Message::Notification(notification)) => event(notification),
                    other => panic!("expected an event, got {other:?}"),
                },
            };
            let stop = done(&event);
            events.push(event);
            if stop {
                return events;
            }
        }
    }

    /// Sends a request without waiting for its response, which [`Conn::responses`] collects.
    async fn send<M: RequestMethod>(&mut self, params: M::Params) -> RequestId {
        self.client.send::<M>(params).await
    }

    /// The responses to `ids`, in that order, keeping the events that arrive meanwhile.
    async fn responses(
        &mut self,
        ids: &[RequestId],
    ) -> Vec<Result<serde_json::Value, ErrorObject>> {
        let mut answers: Vec<Option<Result<serde_json::Value, ErrorObject>>> =
            ids.iter().map(|_| None).collect();
        while answers.iter().any(Option::is_none) {
            match self.client.next().await {
                Some(Message::Response(response)) => {
                    let at = ids
                        .iter()
                        .position(|id| response.id.as_ref() == Some(id))
                        .expect("a response to one of the requests");
                    answers[at] = Some(response.into_result());
                }
                Some(Message::Notification(notification)) => {
                    self.pending.push_back(event(notification));
                }
                other => panic!("expected a response, got {other:?}"),
            }
        }
        answers.into_iter().map(Option::unwrap).collect()
    }

    async fn running_agents(&mut self) -> u32 {
        self.call::<HostHealth>(HostHealthParams {})
            .await
            .unwrap()
            .running_agents
    }

    async fn delete(&mut self, run_id: RunId) -> Result<(), ErrorObject> {
        self.call::<ThreadDelete>(ThreadDeleteParams { run_id })
            .await
            .map(|_| ())
    }

    async fn list(&mut self) -> ThreadListResult {
        self.call::<ThreadList>(ThreadListParams {}).await.unwrap()
    }

    async fn add(&mut self, path: &Path) -> Repo {
        self.call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: path.to_str().unwrap().to_owned(),
        })
        .await
        .unwrap()
        .repo
    }
}

fn updated_to(status: AgentStatus) -> impl FnMut(&EventsEventParams) -> bool {
    move |event| matches!(&event.event, ParallaxEvent::AgentUpdated { state, .. } if state.status == status)
}

fn scope(repo: RepoId) -> ProjectId {
    ProjectId::try_from(uuid::Uuid::from(repo)).unwrap()
}

#[tokio::test]
async fn a_thread_runs_in_a_worktree_of_its_repo_entry_and_lists_under_it() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let listed = client.list().await;
    assert!(listed.repos.is_empty() && listed.threads.is_empty());
    client.subscribe(listed.seq, None).await;

    let repo = client.add(&path).await;
    assert_eq!(repo.name, "app");
    assert_eq!(repo.path, path.to_str().unwrap());
    assert!(!repo.scratch);
    let again = client.add(&path).await;
    assert_eq!(again, repo, "a path has one entry");
    let added = client
        .until(|event| matches!(event.event, ParallaxEvent::RepoAdded { .. }))
        .await;
    assert_eq!(added.last().unwrap().project, None, "host-level");

    let params = start_params(Some(repo.id), "Write some notes");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    assert_eq!(started.thread.id, params.run_id);
    assert_eq!(started.thread.repo, repo.id);
    assert!(!started.thread.archived);
    assert_eq!(started.run.project, scope(repo.id));
    let retried = client.call::<ThreadStart>(params.clone()).await.unwrap();
    assert_eq!(retried.thread, started.thread, "idempotent on the run id");
    let worktree = PathBuf::from(started.run.worktree_path.clone().unwrap());
    let branch = started.run.branch.clone().unwrap();
    client
        .until(|event| matches!(&event.event, ParallaxEvent::ThreadStarted { thread } if thread.id == params.run_id))
        .await;

    let mut runs = host.client().await;
    runs.subscribe(0, Some(scope(repo.id))).await;
    let events = runs.until(updated_to(AgentStatus::Completed)).await;
    assert!(
        events
            .iter()
            .all(|event| event.project == Some(scope(repo.id)))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event.event, ParallaxEvent::AgentDiffReady { .. }))
    );
    assert_eq!(
        git(&path, &["show", &format!("{branch}:NOTES.md")]),
        "Written in a thread."
    );
    assert!(
        worktree
            .canonicalize()
            .unwrap()
            .starts_with(host.data().join("worktrees"))
    );
    assert!(
        !path.join("NOTES.md").exists(),
        "the user's checkout is untouched"
    );

    let listed = runs
        .call::<AgentList>(AgentListParams {
            project: Some(scope(repo.id)),
        })
        .await
        .unwrap();
    assert_eq!(listed.runs.len(), 1);
    assert_eq!(listed.runs[0].id, params.run_id);
    let threads = runs.list().await;
    assert_eq!(threads.repos, [repo]);
    assert_eq!(threads.threads, [started.thread]);
}

#[tokio::test]
async fn a_thread_in_the_current_checkout_works_on_the_branch_the_user_has_out() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    git(&path, &["checkout", "-q", "-b", "my-feature"]);
    let head = git(&path, &["rev-parse", "HEAD"]);
    // The user's own uncommitted work, which the thread must neither commit nor lose.
    std::fs::write(path.join("README.md"), "hello, edited by the user\n").unwrap();
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let mut runs = host.client().await;
    runs.subscribe(0, Some(scope(repo.id))).await;

    let params = ThreadStartParams {
        checkout: true,
        branch_slug: Some("not-used".to_owned()),
        ..start_params(Some(repo.id), "Write some notes")
    };
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    assert!(started.run.checkout);
    assert_eq!(started.run.branch, None, "no branch of its own");
    assert_eq!(started.run.worktree_path, None, "no worktree of its own");
    let retried = client.call::<ThreadStart>(params.clone()).await.unwrap();
    assert_eq!(retried.thread, started.thread, "idempotent on the run id");
    let in_worktree = ThreadStartParams {
        checkout: false,
        ..params.clone()
    };
    let conflict = client.call::<ThreadStart>(in_worktree).await.unwrap_err();
    assert_eq!(kind(&conflict), ErrorKind::IdConflict);

    let events = runs.until(updated_to(AgentStatus::Completed)).await;
    let seen = events.last().unwrap().seq;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.event, ParallaxEvent::AgentDiffReady { .. })),
        "nothing is committed, so there is no diff"
    );
    assert_eq!(
        std::fs::read_to_string(path.join("NOTES.md")).unwrap(),
        "Written in a thread.\n",
        "the change lands in the user's checkout"
    );
    assert_eq!(git(&path, &["branch", "--show-current"]), "my-feature");
    assert_eq!(
        git(&path, &["rev-parse", "HEAD"]),
        head,
        "nothing is committed"
    );
    assert_eq!(
        git(&path, &["status", "--porcelain"]),
        "M README.md\n?? NOTES.md",
        "the user's own edit is kept, uncommitted, beside the thread's"
    );
    assert_eq!(
        git(&path, &["worktree", "list", "--porcelain"])
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count(),
        1,
        "no worktree was made"
    );
    assert!(!host.data().join("worktrees").join("app").exists());
    assert_eq!(git(&path, &["branch", "--list", "parallax/*"]), "");

    // After a restart, a message resumes it in the same checkout.
    drop((client, runs));
    let host = host.restart(fake(editing())).await;
    let mut client = host.client().await;
    let mut runs = host.client().await;
    runs.subscribe(seen, Some(scope(repo.id))).await;
    std::fs::remove_file(path.join("NOTES.md")).unwrap();
    client
        .call::<AgentSend>(message(params.run_id, "Write them again"))
        .await
        .unwrap();
    runs.until(updated_to(AgentStatus::Completed)).await;
    assert!(
        path.join("NOTES.md").is_file(),
        "the resumed run wrote here"
    );
    assert_eq!(git(&path, &["rev-parse", "HEAD"]), head);

    // Deleting the thread leaves the checkout and its changes alone.
    client.delete(params.run_id).await.unwrap();
    assert!(path.join("NOTES.md").is_file());
    assert_eq!(git(&path, &["branch", "--show-current"]), "my-feature");
    assert_eq!(
        std::fs::read_to_string(path.join("README.md")).unwrap(),
        "hello, edited by the user\n"
    );
}

#[tokio::test]
async fn a_thread_with_no_repo_has_no_checkout_to_work_in() {
    let host = Host::start(fake(editing()));
    let mut client = host.client().await;
    let params = ThreadStartParams {
        checkout: true,
        ..start_params(None, "Jot something down")
    };
    let error = client.call::<ThreadStart>(params).await.unwrap_err();
    assert_eq!(error.code, INVALID_PARAMS);
    assert!(client.list().await.threads.is_empty());
}

#[tokio::test]
async fn a_thread_with_no_repo_gets_its_own_scratch_repository() {
    let host = Host::start(fake(editing()));
    let mut client = host.client().await;
    client.subscribe(0, None).await;

    let params = start_params(None, "Jot something down");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let events = client
        .until(|event| matches!(event.event, ParallaxEvent::ThreadStarted { .. }))
        .await;
    let scratch = events
        .iter()
        .find_map(|event| match &event.event {
            ParallaxEvent::RepoAdded { repo } => Some(repo.clone()),
            _ => None,
        })
        .expect("the scratch entry is announced");
    assert!(scratch.scratch);
    assert_eq!(started.thread.repo, scratch.id);
    assert_eq!(PathBuf::from(&scratch.path), host.data().join("scratch"));

    let repository = host.data().join("scratch").join(params.run_id.to_string());
    let mut runs = host.client().await;
    runs.subscribe(0, Some(scope(scratch.id))).await;
    runs.until(updated_to(AgentStatus::Completed)).await;
    let branch = started.run.branch.unwrap();
    assert_eq!(
        git(&repository, &["log", "--format=%an %s", &branch]),
        "parallax parallax: Jot something down\nparallax Start a parallax scratch folder"
    );

    let second = start_params(None, "Another quick chat");
    let other = client.call::<ThreadStart>(second.clone()).await.unwrap();
    assert_eq!(other.thread.repo, scratch.id, "one scratch entry");
    assert!(
        host.data()
            .join("scratch")
            .join(second.run_id.to_string())
            .join(".git")
            .is_dir(),
        "each thread has its own repository"
    );
    let listed = client.list().await;
    assert_eq!(listed.repos, [scratch]);
    assert_eq!(listed.threads.len(), 2);
}

#[tokio::test]
async fn deleting_a_running_thread_stops_its_agent_and_removes_everything() {
    let host = Host::start(fake(hang()));
    let mut client = host.client().await;
    let params = start_params(None, "Wait for me");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let repository = host.data().join("scratch").join(params.run_id.to_string());
    let context = host.data().join("context").join(params.run_id.to_string());
    let worktree = PathBuf::from(started.run.worktree_path.unwrap());
    client.subscribe(0, None).await;
    client.subscribe(0, Some(scope(started.thread.repo))).await;
    client.until(updated_to(AgentStatus::Running)).await;
    assert!(context.is_dir(), "a thread with no repo has its own notes");
    assert!(
        !host
            .data()
            .join("context")
            .join(started.thread.repo.to_string())
            .exists(),
        "quick chats share no notes"
    );

    let archived = client
        .call::<ThreadArchive>(ThreadArchiveParams {
            run_id: params.run_id,
            archived: true,
        })
        .await
        .unwrap()
        .thread;
    assert!(archived.archived);
    client
        .until(
            |event| matches!(&event.event, ParallaxEvent::ThreadUpdated { thread } if thread.archived),
        )
        .await;
    assert!(client.list().await.threads[0].archived);

    client.delete(params.run_id).await.unwrap();
    client
        .until(|event| matches!(&event.event, ParallaxEvent::ThreadDeleted { run_id, .. } if *run_id == params.run_id))
        .await;
    assert_eq!(client.running_agents().await, 0, "the agent was stopped");

    assert!(client.list().await.threads.is_empty());
    let runs = client
        .call::<AgentList>(AgentListParams {
            project: Some(scope(started.thread.repo)),
        })
        .await
        .unwrap()
        .runs;
    assert!(runs.is_empty());
    let events = client
        .call::<AgentEvents>(AgentEventsParams {
            run_id: params.run_id,
            after: 0,
            limit: None,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&events), ErrorKind::RunNotFound);
    assert!(!worktree.exists(), "the worktree is removed");
    assert!(!repository.exists(), "the scratch repository is removed");
    assert!(!context.exists(), "the thread's notes are removed");

    let mut replay = host.client().await;
    replay.subscribe(0, Some(scope(started.thread.repo))).await;
    let health = replay.running_agents().await;
    assert_eq!(health, 0);
    assert!(
        replay
            .pending
            .iter()
            .all(|event| event.project != Some(scope(started.thread.repo))),
        "a deleted thread's transcript isn't replayed: {:#?}",
        replay.pending
    );

    let missing = client.delete(params.run_id).await.unwrap_err();
    assert_eq!(kind(&missing), ErrorKind::ThreadNotFound);
}

#[tokio::test]
async fn a_delete_racing_a_message_to_a_finished_thread_leaves_nothing_running() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let params = start_params(Some(repo.id), "Write some notes");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let worktree = PathBuf::from(started.run.worktree_path.unwrap());
    let branch = started.run.branch.unwrap();
    client.subscribe(0, Some(scope(repo.id))).await;
    client.until(updated_to(AgentStatus::Completed)).await;

    let send = client
        .send::<AgentSend>(message(params.run_id, "And some more"))
        .await;
    let delete = client
        .send::<ThreadDelete>(ThreadDeleteParams {
            run_id: params.run_id,
        })
        .await;
    let answers = client.responses(&[send, delete]).await;
    if let Err(error) = &answers[0] {
        assert_eq!(kind(error), ErrorKind::RunNotFound, "{error:?}");
    }
    answers[1].as_ref().expect("the delete succeeds");

    assert_eq!(client.running_agents().await, 0);
    assert!(client.list().await.threads.is_empty());
    let runs = client
        .call::<AgentList>(AgentListParams {
            project: Some(scope(repo.id)),
        })
        .await
        .unwrap()
        .runs;
    assert!(runs.is_empty());
    assert!(!worktree.exists(), "the worktree is removed");
    let branches = git(&path, &["branch", "--list", &branch]);
    assert!(branches.is_empty(), "the branch is removed: {branches}");
    let after = client
        .send::<AgentSend>(message(params.run_id, "Still there?"))
        .await;
    let answer = client.responses(&[after]).await.remove(0).unwrap_err();
    assert_eq!(kind(&answer), ErrorKind::RunNotFound);
}

#[tokio::test]
async fn an_accepted_quick_chat_deletes_its_scratch_repository() {
    let host = Host::start(fake(editing()));
    let mut client = host.client().await;
    let params = start_params(None, "Jot something down");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let repository = host.data().join("scratch").join(params.run_id.to_string());
    client.subscribe(0, Some(scope(started.thread.repo))).await;
    client.until(updated_to(AgentStatus::Completed)).await;
    client
        .call::<AgentAccept>(AgentAcceptParams {
            run_id: params.run_id,
            id: AcceptId::generate(),
            commit: None,
        })
        .await
        .unwrap();
    assert!(repository.is_dir());

    client.delete(params.run_id).await.unwrap();
    assert!(
        !repository.exists(),
        "removed although the accept removed the worktree row"
    );
}

#[tokio::test]
async fn a_retried_start_on_a_taken_run_id_makes_no_scratch_repository() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let params = start_params(Some(repo.id), "Write some notes");
    client.call::<ThreadStart>(params.clone()).await.unwrap();

    let conflict = client
        .call::<ThreadStart>(ThreadStartParams {
            repo: None,
            ..params.clone()
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&conflict), ErrorKind::IdConflict);
    assert!(
        !host
            .data()
            .join("scratch")
            .join(params.run_id.to_string())
            .exists()
    );
}

#[tokio::test]
async fn repo_entries_and_threads_refuse_what_they_cant_run() {
    let host = Host::start(fake(editing()));
    let mut client = host.client().await;
    let plain = host.work.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let error = client
        .call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: plain.to_str().unwrap().to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::NotARepository);
    let relative = client
        .call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: "repos/app".to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!(relative.code, INVALID_PARAMS);

    let notes = host.data().join("context").join("planted");
    std::fs::create_dir_all(&notes).unwrap();
    git(&notes, &["init", "-q"]);
    let inside = client
        .call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: notes.to_str().unwrap().to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&inside), ErrorKind::NotARepository, "{inside:?}");
    let link = host.work.path().join("link");
    std::os::unix::fs::symlink(&notes, &link).unwrap();
    let linked = client
        .call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: link.to_str().unwrap().to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!(
        kind(&linked),
        ErrorKind::NotARepository,
        "a link into the data folder"
    );

    let path = real_repo(host.work.path(), "app");
    let other = real_repo(host.work.path(), "other");
    let id = RepoId::generate();
    client
        .call::<RepoAdd>(RepoAddParams {
            id,
            path: path.to_str().unwrap().to_owned(),
        })
        .await
        .unwrap();
    let conflict = client
        .call::<RepoAdd>(RepoAddParams {
            id,
            path: other.to_str().unwrap().to_owned(),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&conflict), ErrorKind::IdConflict);

    let unknown = client
        .call::<ThreadStart>(start_params(Some(RepoId::generate()), "Anything"))
        .await
        .unwrap_err();
    assert_eq!(kind(&unknown), ErrorKind::RepoNotFound);
    let empty = client
        .call::<ThreadStart>(start_params(None, "  "))
        .await
        .unwrap_err();
    assert_eq!(empty.code, INVALID_PARAMS);
    let archive = client
        .call::<ThreadArchive>(ThreadArchiveParams {
            run_id: RunId::generate(),
            archived: true,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&archive), ErrorKind::ThreadNotFound);
}

/// `thread/start`'s `branchSlug` names the worktree branch, and an invalid one is refused.
#[tokio::test]
async fn a_thread_can_name_its_branch() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;

    let started = client
        .call::<ThreadStart>(ThreadStartParams {
            branch_slug: Some("write-some-notes".to_owned()),
            ..start_params(Some(repo.id), "Write some notes")
        })
        .await
        .unwrap();
    assert_eq!(
        started.run.branch.as_deref(),
        Some("parallax/write-some-notes")
    );

    let refused = client
        .call::<ThreadStart>(ThreadStartParams {
            branch_slug: Some("../escape".to_owned()),
            ..start_params(Some(repo.id), "Write more notes")
        })
        .await
        .unwrap_err();
    assert_eq!(refused.code, INVALID_PARAMS, "{refused:?}");
}

/// Commits a change to `file` in `repo`, committed at `date`.
fn commit_at(repo: &Path, file: &str, date: &str) {
    std::fs::write(repo.join(file), date).unwrap();
    git(repo, &["add", "-A"]);
    let output = Command::new("git")
        .args(["commit", "-q", "-m", file])
        .current_dir(repo)
        .env("GIT_COMMITTER_DATE", date)
        .output()
        .expect("git runs");
    assert!(output.status.success(), "{output:?}");
}

/// `repo/refs` lists the default branch first, then the rest newest first, with what each is.
#[tokio::test]
async fn repo_refs_lists_the_default_branch_first_then_the_newest() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    git(&path, &["checkout", "-q", "-b", "develop"]);
    commit_at(&path, "DEV.md", "2021-01-01T00:00:00Z");
    git(&path, &["checkout", "-q", "-b", "older", "main"]);
    commit_at(&path, "OLD.md", "2020-01-01T00:00:00Z");
    git(&path, &["checkout", "-q", "main"]);
    git(
        &path,
        &["update-ref", "refs/remotes/origin/develop", "develop"],
    );
    git(
        &path,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/develop",
        ],
    );
    let elsewhere = host.work.path().join("elsewhere");
    git(
        &path,
        &[
            "worktree",
            "add",
            "-q",
            elsewhere.to_str().unwrap(),
            "older",
        ],
    );
    let mut client = host.client().await;
    let repo = client.add(&path).await;

    let refs = client
        .call::<RepoRefs>(RepoRefsParams { repo: repo.id })
        .await
        .unwrap()
        .refs;
    let shown: Vec<_> = refs
        .iter()
        .map(|r| (r.name.as_str(), r.remote, r.default, r.current, r.worktree))
        .collect();
    assert_eq!(
        shown,
        [
            ("develop", false, true, false, false),
            ("main", false, false, true, false),
            ("origin/develop", true, false, false, false),
            ("older", false, false, false, true),
        ]
    );

    let unknown = client
        .call::<RepoRefs>(RepoRefsParams {
            repo: RepoId::generate(),
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&unknown), ErrorKind::RepoNotFound);
}

/// `thread/start`'s `base` starts the worktree from that ref, and a bad one is refused.
#[tokio::test]
async fn a_thread_starts_its_worktree_from_the_ref_it_picks() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    git(&path, &["checkout", "-q", "-b", "develop"]);
    commit_at(&path, "DEV.md", "2021-01-01T00:00:00Z");
    git(&path, &["checkout", "-q", "main"]);
    let mut client = host.client().await;
    let repo = client.add(&path).await;

    let started = client
        .call::<ThreadStart>(ThreadStartParams {
            base: Some("develop".to_owned()),
            ..start_params(Some(repo.id), "Write some notes")
        })
        .await
        .unwrap();
    let worktree = PathBuf::from(started.run.worktree_path.unwrap());
    assert!(worktree.join("DEV.md").is_file(), "it starts from develop");
    assert_eq!(git(&path, &["branch", "--show-current"]), "main");

    for params in [
        ThreadStartParams {
            base: Some("--orphan".to_owned()),
            ..start_params(Some(repo.id), "Write more notes")
        },
        ThreadStartParams {
            base: Some("develop".to_owned()),
            checkout: true,
            ..start_params(Some(repo.id), "Write more notes")
        },
        ThreadStartParams {
            checkout_ref: Some("develop".to_owned()),
            ..start_params(Some(repo.id), "Write more notes")
        },
    ] {
        let refused = client.call::<ThreadStart>(params).await.unwrap_err();
        assert_eq!(refused.code, INVALID_PARAMS, "{refused:?}");
    }
}

/// `thread/start`'s `checkoutRef` switches the checkout first, a remote-tracking ref to a local
/// branch that tracks it, and never over the user's changes.
#[tokio::test]
async fn a_checkout_thread_switches_to_the_ref_it_picks_but_never_over_changes() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    git(&path, &["checkout", "-q", "-b", "feature"]);
    commit_at(&path, "README.md", "2021-01-01T00:00:00Z");
    git(&path, &["checkout", "-q", "main"]);
    // Never fetched: git only needs the remote's refspec to set up tracking.
    git(
        &path,
        &["remote", "add", "origin", "https://example.invalid/app.git"],
    );
    git(
        &path,
        &["update-ref", "refs/remotes/origin/topic", "feature"],
    );
    git(&path, &["branch", "-q", "-D", "feature"]);
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let mut runs = host.client().await;
    runs.subscribe(0, Some(scope(repo.id))).await;

    let params = ThreadStartParams {
        checkout: true,
        checkout_ref: Some("origin/topic".to_owned()),
        ..start_params(Some(repo.id), "Write some notes")
    };
    client.call::<ThreadStart>(params.clone()).await.unwrap();
    assert_eq!(git(&path, &["branch", "--show-current"]), "topic");
    assert_eq!(
        git(&path, &["rev-parse", "--abbrev-ref", "topic@{upstream}"]),
        "origin/topic"
    );
    runs.until(updated_to(AgentStatus::Completed)).await;

    // The user's edit to a file `main` has otherwise: git refuses, and nothing starts.
    std::fs::write(path.join("README.md"), "the user's edit\n").unwrap();
    let refused = client
        .call::<ThreadStart>(ThreadStartParams {
            checkout: true,
            checkout_ref: Some("main".to_owned()),
            ..start_params(Some(repo.id), "Write more notes")
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&refused), ErrorKind::WorktreeFailed);
    assert!(refused.message.contains("overwritten"), "{refused:?}");
    assert_eq!(git(&path, &["branch", "--show-current"]), "topic");
    assert_eq!(
        std::fs::read_to_string(path.join("README.md")).unwrap(),
        "the user's edit\n"
    );
    assert_eq!(client.list().await.threads.len(), 1);
}

/// A `checkoutRef` never switches a checkout another thread is running in, which would move that
/// thread's work to a branch it never chose.
#[tokio::test]
async fn a_checkout_thread_never_switches_under_a_running_one() {
    let host = Host::start(fake(hang()));
    let path = real_repo(host.work.path(), "app");
    git(&path, &["branch", "-q", "other"]);
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let mut runs = host.client().await;
    runs.subscribe(0, Some(scope(repo.id))).await;
    client
        .call::<ThreadStart>(ThreadStartParams {
            checkout: true,
            ..start_params(Some(repo.id), "Keep working")
        })
        .await
        .unwrap();
    runs.until(updated_to(AgentStatus::Running)).await;

    let refused = client
        .call::<ThreadStart>(ThreadStartParams {
            checkout: true,
            checkout_ref: Some("other".to_owned()),
            ..start_params(Some(repo.id), "Switch away")
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&refused), ErrorKind::WorktreeFailed);
    assert_eq!(git(&path, &["branch", "--show-current"]), "main");
    assert_eq!(client.list().await.threads.len(), 1);
}

/// RYA-276, 0034: a thread's CLI gets the user's message as is, with no Parallax limits before it,
/// as in Claude Code.
#[tokio::test]
async fn a_threads_first_message_is_the_users_own() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let mut backends = BackendRegistry::new();
    backends.register(
        Provider::Anthropic,
        Arc::new(Other::new(fake_backend(editing()), &prompts)),
    );
    let host = Host::start(backends);
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    client.subscribe(0, Some(scope(repo.id))).await;
    client
        .call::<ThreadStart>(ThreadStartParams {
            account: Some(AccountChoice::Subscription {
                backend: "other".to_owned(),
            }),
            ..start_params(Some(repo.id), "Write the notes")
        })
        .await
        .unwrap();
    client.until(updated_to(AgentStatus::Completed)).await;
    assert_eq!(
        *prompts.lock().unwrap(),
        [("Write the notes".to_owned(), false)]
    );
    host.server.stop().await;
}

/// RYA-97, RYA-222: a thread's model, effort, permission, and approvals reach its backend when it
/// starts and when it resumes, come back on its run, and count for `thread/start`'s idempotency.
/// What the backend can't honor is refused before anything is made.
#[tokio::test]
async fn a_thread_keeps_its_model_effort_permission_and_approvals() {
    let (host, seen) = with_options(editing());
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let mut runs = host.client().await;
    runs.subscribe(0, Some(scope(repo.id))).await;

    let params = ThreadStartParams {
        model: Some("opus".to_owned()),
        effort: Some(AgentEffort::High),
        permission: Some(AgentPermission::Plan),
        approvals: true,
        ..start_params(Some(repo.id), "Plan the notes")
    };
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    assert_eq!(started.run.model.as_deref(), Some("opus"));
    assert_eq!(started.run.effort, Some(AgentEffort::High));
    assert_eq!(started.run.permission, Some(AgentPermission::Plan));
    assert!(started.run.approvals);
    runs.until(updated_to(AgentStatus::Completed)).await;

    // The CLI has exited, so a message resumes the run, with the same options.
    client
        .call::<AgentSend>(message(params.run_id, "And a summary"))
        .await
        .unwrap();
    runs.until(updated_to(AgentStatus::Completed)).await;
    let options: Options = (
        Some("opus".to_owned()),
        Some(AgentEffort::High),
        Some(AgentPermission::Plan),
        true,
        true,
    );
    assert_eq!(*seen.lock().unwrap(), [options.clone(), options]);

    let retry = client.call::<ThreadStart>(params.clone()).await.unwrap();
    assert_eq!(retry.run.id, started.run.id);
    for changed in [
        ThreadStartParams {
            effort: None,
            ..params.clone()
        },
        ThreadStartParams {
            approvals: false,
            ..params
        },
    ] {
        let conflict = client.call::<ThreadStart>(changed).await.unwrap_err();
        assert_eq!(kind(&conflict), ErrorKind::IdConflict);
    }

    for refused in [
        ThreadStartParams {
            effort: Some(AgentEffort::Max),
            ..start_params(None, "Anything")
        },
        ThreadStartParams {
            permission: Some(AgentPermission::Edit),
            ..start_params(None, "Anything")
        },
        ThreadStartParams {
            model: Some("--dangerously-skip-permissions".to_owned()),
            ..start_params(None, "Anything")
        },
        ThreadStartParams {
            context_window: Some(200_000),
            ..start_params(None, "Anything")
        },
        ThreadStartParams {
            fast: Some(true),
            ..start_params(None, "Anything")
        },
    ] {
        let run_id = refused.run_id;
        let error = client.call::<ThreadStart>(refused).await.unwrap_err();
        assert_eq!(kind(&error), ErrorKind::UnsupportedOption, "{error:?}");
        let scratch = host.data().join("scratch").join(run_id.to_string());
        assert!(!scratch.exists(), "no scratch repository is left behind");
    }
    assert_eq!(
        client.list().await.threads.len(),
        1,
        "no other thread was made"
    );
    assert_eq!(seen.lock().unwrap().len(), 2, "no other run started");
}

/// RYA-161, RYA-163: a message to a finished thread can change its model and effort. The change
/// is stored, reported on its run and on `agent.updated`, and used by the resumed CLI. An effort
/// the backend can't honor is refused before anything changes.
#[tokio::test]
async fn a_message_changes_a_finished_threads_model_and_effort() {
    let (host, seen) = with_options(editing());
    let mut client = host.client().await;
    let params = ThreadStartParams {
        model: Some("opus".to_owned()),
        effort: Some(AgentEffort::High),
        permission: Some(AgentPermission::Plan),
        ..start_params(None, "Plan the notes")
    };
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let scope = scope(started.thread.repo);
    let mut runs = host.client().await;
    runs.subscribe(0, Some(scope)).await;
    runs.until(updated_to(AgentStatus::Completed)).await;

    let refused = client
        .call::<AgentSend>(AgentSendParams {
            model: Some("sonnet".to_owned()),
            effort: Some(AgentEffort::Max),
            ..message(params.run_id, "Think harder")
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&refused), ErrorKind::UnsupportedOption, "{refused:?}");

    // The run's own permission changes nothing.
    let sent = client
        .call::<AgentSend>(AgentSendParams {
            model: Some("sonnet".to_owned()),
            effort: Some(AgentEffort::Low),
            permission: Some(AgentPermission::Plan),
            ..message(params.run_id, "Just a summary")
        })
        .await
        .unwrap()
        .run;
    assert_eq!(sent.model.as_deref(), Some("sonnet"));
    assert_eq!(sent.effort, Some(AgentEffort::Low));
    assert_eq!(sent.permission, Some(AgentPermission::Plan));
    let updates: Vec<_> = runs
        .until(updated_to(AgentStatus::Completed))
        .await
        .into_iter()
        .filter_map(|event| match event.event {
            ParallaxEvent::AgentUpdated { state, .. } => Some((state.model, state.effort)),
            _ => None,
        })
        .collect();
    assert!(
        !updates.is_empty()
            && updates
                .iter()
                .all(|u| *u == (Some("sonnet".to_owned()), Some(AgentEffort::Low))),
        "agent.updated reports the new model and effort: {updates:?}"
    );
    let opus = Some("opus".to_owned());
    let sonnet = Some("sonnet".to_owned());
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (
                opus,
                Some(AgentEffort::High),
                Some(AgentPermission::Plan),
                false,
                true
            ),
            (
                sonnet,
                Some(AgentEffort::Low),
                Some(AgentPermission::Plan),
                false,
                true
            ),
        ]
    );
    let listed = client
        .call::<AgentList>(AgentListParams {
            project: Some(scope),
        })
        .await
        .unwrap()
        .runs;
    assert_eq!(listed[0].model.as_deref(), Some("sonnet"), "it was stored");
    assert_eq!(listed[0].effort, Some(AgentEffort::Low), "it was stored");
}

/// A running CLI can't change its model or effort, so a message asking for another one waits,
/// with every message sent after it, until the CLI exits; then each starts a CLI of its own, in
/// order, the first with the new model.
#[tokio::test]
async fn a_message_with_a_new_model_waits_for_a_running_thread_to_finish() {
    let slow = vec![
        Step::Init {
            session_id: "slow-1".to_owned(),
            model: None,
        },
        Step::SleepMs(1000),
        Step::Emit(Event::Text {
            message_id: None,
            text: "Done.".to_owned(),
        }),
        Step::EndTurn {
            result: Some("Done.".to_owned()),
        },
    ];
    let (host, seen) = with_options(slow);
    let mut client = host.client().await;
    let params = ThreadStartParams {
        effort: Some(AgentEffort::High),
        ..start_params(None, "Take your time")
    };
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let scope = scope(started.thread.repo);
    client.subscribe(0, Some(scope)).await;
    client.until(updated_to(AgentStatus::Running)).await;

    let change = AgentSendParams {
        model: Some("sonnet".to_owned()),
        ..message(params.run_id, "Hurry up")
    };
    let waiting = client.call::<AgentSend>(change.clone()).await.unwrap().run;
    assert_eq!(waiting.status, AgentStatus::Running);
    assert_eq!(waiting.model, None, "nothing changes until the CLI exits");
    let after = message(params.run_id, "And the tests");
    client.call::<AgentSend>(after.clone()).await.unwrap();
    // A retry of a waiting message is the same message.
    client.call::<AgentSend>(change.clone()).await.unwrap();
    let conflict = client
        .call::<AgentSend>(AgentSendParams {
            text: "Something else".to_owned(),
            ..change.clone()
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&conflict), ErrorKind::IdConflict, "{conflict:?}");
    assert_eq!(seen.lock().unwrap().len(), 1, "no other CLI started yet");

    // The first CLI, then one for each waiting message.
    let mut events = Vec::new();
    for _ in 0..3 {
        events.extend(client.until(updated_to(AgentStatus::Completed)).await);
    }
    let turns: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.event {
            ParallaxEvent::AgentOutput { items, .. } => Some(items),
            _ => None,
        })
        .flatten()
        .filter_map(|item| match item {
            parallax_protocol::AgentOutputItem::TurnStarted {
                turn_id: Some(turn_id),
                ..
            } => Some(*turn_id),
            _ => None,
        })
        .collect();
    assert_eq!(turns, [change.turn_id, after.turn_id], "in the order sent");
    let sonnet = Some("sonnet".to_owned());
    let high = Some(AgentEffort::High);
    assert_eq!(
        *seen.lock().unwrap(),
        [
            (None, high, None, false, true),
            (sonnet.clone(), high, None, false, true),
            (sonnet, high, None, false, true),
        ]
    );
    host.server.stop().await;
}

/// The fake CLI under another name, as another provider's backend, recording each prompt it gets
/// and whether it resumed a session. Each start takes the next of `first`, where `None` refuses
/// to start, then `fake` once `first` is empty.
struct Other {
    fake: FakeBackend,
    first: Mutex<VecDeque<Option<FakeBackend>>>,
    prompts: Arc<Mutex<Vec<(String, bool)>>>,
}

impl Other {
    fn new(fake: FakeBackend, prompts: &Arc<Mutex<Vec<(String, bool)>>>) -> Self {
        Self {
            fake,
            first: Mutex::new(VecDeque::new()),
            prompts: Arc::clone(prompts),
        }
    }
}

impl Backend for Other {
    fn name(&self) -> &'static str {
        "other"
    }

    fn capabilities(&self) -> Capabilities {
        self.fake.capabilities()
    }

    fn start(&self, request: RunRequest) -> Result<Started, StartError> {
        let prompt = (request.prompt.clone(), request.resume.is_some());
        self.prompts.lock().unwrap().push(prompt);
        match self.first.lock().unwrap().pop_front() {
            Some(Some(fake)) => fake.start(request),
            Some(None) => Err(StartError::Unsupported("refused".to_owned())),
            None => self.fake.start(request),
        }
    }
}

/// A message on another backend's account moves the thread there: the same run, worktree, and
/// transcript, on a new session that's told the conversation so far.
#[tokio::test]
async fn a_message_on_another_backends_account_moves_the_thread_there() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let mut backends = fake(editing());
    backends.register(
        Provider::Openai,
        Arc::new(Other::new(fake_backend(editing()), &prompts)),
    );
    let host = Host::start(backends);
    let mut client = host.client().await;
    let params = start_params(None, "Write the notes");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let scope = scope(started.thread.repo);
    client.subscribe(0, Some(scope)).await;
    client.until(updated_to(AgentStatus::Completed)).await;

    let moved = client
        .call::<AgentSend>(AgentSendParams {
            model: Some("gpt-6".to_owned()),
            account: Some(AccountChoice::Subscription {
                backend: "other".to_owned(),
            }),
            ..message(params.run_id, "Now the tests")
        })
        .await
        .unwrap()
        .run;
    assert_eq!(moved.id, params.run_id, "the same thread");
    assert_eq!(moved.backend, "other");
    assert_eq!(moved.account_id, "other");
    assert_eq!(moved.model.as_deref(), Some("gpt-6"));
    assert_eq!(moved.branch, started.run.branch, "the same worktree");
    let events = client.until(updated_to(AgentStatus::Completed)).await;
    assert!(
        events.iter().any(|event| matches!(
            &event.event,
            ParallaxEvent::AgentUpdated { state, .. } if state.backend.as_deref() == Some("other")
        )),
        "agent.updated reports the move: {events:?}"
    );
    let prompts = prompts.lock().unwrap().clone();
    let [(prompt, resumed)] = prompts.as_slice() else {
        panic!("one CLI started on the other backend: {prompts:?}");
    };
    assert!(!resumed, "a new session");
    assert!(
        prompt.contains("User:\nWrite the notes\n\nAgent:\nDone.\n</conversation>"),
        "{prompt}"
    );
    assert!(
        prompt.ends_with("which is yours to answer:\nNow the tests"),
        "{prompt}"
    );
    host.server.stop().await;
}

/// A message on another backend's account moves a Current checkout thread there, still in the
/// user's checkout, with the conversation as its first message.
#[tokio::test]
async fn a_checkout_thread_moves_to_another_backend_in_the_same_checkout() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let mut backends = fake(editing());
    backends.register(
        Provider::Openai,
        Arc::new(Other::new(fake_backend(editing()), &prompts)),
    );
    let host = Host::start(backends);
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    client.subscribe(0, Some(scope(repo.id))).await;
    let params = ThreadStartParams {
        checkout: true,
        ..start_params(Some(repo.id), "Write the notes")
    };
    client.call::<ThreadStart>(params.clone()).await.unwrap();
    client.until(updated_to(AgentStatus::Completed)).await;
    std::fs::remove_file(path.join("NOTES.md")).unwrap();

    let moved = client
        .call::<AgentSend>(AgentSendParams {
            account: Some(AccountChoice::Subscription {
                backend: "other".to_owned(),
            }),
            ..message(params.run_id, "Write them again")
        })
        .await
        .unwrap()
        .run;
    assert_eq!(moved.backend, "other");
    assert!(moved.checkout);
    client.until(updated_to(AgentStatus::Completed)).await;
    assert!(
        path.join("NOTES.md").is_file(),
        "the new CLI wrote in the checkout"
    );
    let prompts = prompts.lock().unwrap().clone();
    let [(prompt, false)] = prompts.as_slice() else {
        panic!("one new session on the other backend: {prompts:?}");
    };
    // A thread's handoff, like its first message, adds no Parallax limits (0034).
    assert!(
        prompt.starts_with("This conversation began with another agent"),
        "{prompt}"
    );
    host.server.stop().await;
}

/// When the other backend's CLI doesn't start, the thread moves back, and its next message
/// resumes its original session.
#[tokio::test]
async fn a_thread_moves_back_when_the_other_backends_cli_does_not_start() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let other = Other::new(fake_backend(editing()), &prompts);
    other.first.lock().unwrap().push_back(None);
    let mut backends = fake(editing());
    backends.register(Provider::Openai, Arc::new(other));
    let host = Host::start(backends);
    let mut client = host.client().await;
    let params = start_params(None, "Write the notes");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    client.subscribe(0, Some(scope(started.thread.repo))).await;
    client.until(updated_to(AgentStatus::Completed)).await;

    let refused = client
        .call::<AgentSend>(AgentSendParams {
            model: Some("gpt-6".to_owned()),
            account: Some(AccountChoice::Subscription {
                backend: "other".to_owned(),
            }),
            ..message(params.run_id, "Now the tests")
        })
        .await
        .unwrap()
        .run;
    assert_eq!(refused.status, AgentStatus::Failed, "{refused:?}");
    assert_eq!(refused.backend, started.run.backend, "moved back");
    assert_eq!(refused.model, started.run.model);
    assert_eq!(
        refused.session_id.as_deref(),
        Some("thread-1"),
        "its session is kept"
    );

    let resumed = client
        .call::<AgentSend>(message(params.run_id, "Try again"))
        .await
        .unwrap()
        .run;
    assert_eq!(resumed.backend, started.run.backend);
    client.until(updated_to(AgentStatus::Completed)).await;
    assert_eq!(prompts.lock().unwrap().len(), 1, "only the refused start");
    host.server.stop().await;
}

/// A thread whose new CLI on another backend exits before reporting a session isn't stuck: its
/// next message on that backend starts another new session there.
#[tokio::test]
async fn a_moved_thread_with_no_session_starts_a_new_one_on_its_next_message() {
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let other = Other::new(fake_backend(editing()), &prompts);
    let quits = fake_backend(vec![Step::Stderr("not logged in".to_owned())]);
    other.first.lock().unwrap().push_back(Some(quits));
    let mut backends = fake(editing());
    backends.register(Provider::Openai, Arc::new(other));
    let host = Host::start(backends);
    let mut client = host.client().await;
    let params = start_params(None, "Write the notes");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    client.subscribe(0, Some(scope(started.thread.repo))).await;
    client.until(updated_to(AgentStatus::Completed)).await;

    client
        .call::<AgentSend>(AgentSendParams {
            account: Some(AccountChoice::Subscription {
                backend: "other".to_owned(),
            }),
            ..message(params.run_id, "Now the tests")
        })
        .await
        .unwrap();
    client
        .until(|event| {
            matches!(&event.event, ParallaxEvent::AgentUpdated { state, .. }
                if matches!(state.status, AgentStatus::Completed | AgentStatus::Failed))
        })
        .await;

    let again = client
        .call::<AgentSend>(message(params.run_id, "Try again"))
        .await
        .unwrap()
        .run;
    assert_eq!(again.backend, "other");
    client.until(updated_to(AgentStatus::Completed)).await;
    let prompts = prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(!prompts[1].1, "a new session");
    assert!(prompts[1].0.contains("<conversation>"), "{}", prompts[1].0);
    host.server.stop().await;
}

/// Stopping a thread drops the messages waiting for its CLI to exit, which never reached it, and
/// starts no other CLI.
#[tokio::test]
async fn stopping_a_running_thread_drops_the_messages_waiting_for_it() {
    let (host, seen) = with_options(hang());
    let mut client = host.client().await;
    let params = start_params(None, "Wait for me");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let scope = scope(started.thread.repo);
    client.subscribe(0, Some(scope)).await;
    client.until(updated_to(AgentStatus::Running)).await;

    let change = AgentSendParams {
        effort: Some(AgentEffort::Low),
        ..message(params.run_id, "Hurry up")
    };
    client.call::<AgentSend>(change.clone()).await.unwrap();
    client
        .call::<AgentCancel>(AgentCancelParams {
            run_id: params.run_id,
            from: None,
        })
        .await
        .unwrap();
    let events = client.until(updated_to(AgentStatus::Cancelled)).await;
    assert!(
        events.iter().any(|event| matches!(
            &event.event,
            ParallaxEvent::AgentOutput { items, .. } if items.iter().any(|item| matches!(
                item,
                parallax_protocol::AgentOutputItem::FollowUpDropped { turn_id }
                    if *turn_id == change.turn_id
            ))
        )),
        "the waiting message was dropped: {events:?}"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(seen.lock().unwrap().len(), 1, "no other CLI started");
    let listed = client
        .call::<AgentList>(AgentListParams {
            project: Some(scope),
        })
        .await
        .unwrap()
        .runs;
    assert_eq!(listed[0].effort, None, "nothing changed");
    host.server.stop().await;
}

/// A repo entry's icon takes an uploaded image (0038), with `repo.updated`, refuses one over the
/// cap, and loses it to an icon sent without one.
#[tokio::test]
async fn a_repo_icon_takes_an_image_and_a_glyph_clears_it() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let seq = client.list().await.seq;
    client.subscribe(seq, None).await;

    let glyph = ProjectIcon {
        name: "flame".to_owned(),
        color: Some("orange".to_owned()),
        image: None,
    };
    let with_image = ProjectIcon {
        image: Some(PromptImage {
            media_type: ImageMediaType::Png,
            data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==".to_owned(),
        }),
        ..glyph.clone()
    };
    let updated = client
        .call::<RepoUpdate>(RepoUpdateParams {
            repo: repo.id,
            icon: with_image.clone(),
        })
        .await
        .unwrap()
        .repo;
    assert_eq!(updated.icon, Some(with_image.clone()));
    let events = client
        .until(|event| matches!(&event.event, ParallaxEvent::RepoUpdated { .. }))
        .await;
    assert_eq!(
        events.last().unwrap().event,
        ParallaxEvent::RepoUpdated {
            repo: updated.clone()
        }
    );
    assert_eq!(client.list().await.repos, std::slice::from_ref(&updated));

    let too_large = client
        .call::<RepoUpdate>(RepoUpdateParams {
            repo: repo.id,
            icon: ProjectIcon {
                image: Some(PromptImage {
                    media_type: ImageMediaType::Png,
                    data: "A".repeat(64 * 1024 + 4),
                }),
                ..glyph.clone()
            },
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&too_large), ErrorKind::ImageTooLarge);

    let cleared = client
        .call::<RepoUpdate>(RepoUpdateParams {
            repo: repo.id,
            icon: glyph.clone(),
        })
        .await
        .unwrap()
        .repo;
    assert_eq!(
        cleared.icon,
        Some(glyph),
        "an icon without an image clears it"
    );
    let events = client
        .until(|event| matches!(&event.event, ParallaxEvent::RepoUpdated { .. }))
        .await;
    assert_eq!(
        events.last().unwrap().event,
        ParallaxEvent::RepoUpdated {
            repo: cleared.clone()
        }
    );
    assert_eq!(client.list().await.repos, [cleared]);
    host.server.stop().await;
}

/// The sidebar's attention state (0033): a thread is marked seen and snoozed, a message moves its
/// `lastPromptAt`, and a repo entry takes an icon, each with a host-level event.
#[tokio::test]
async fn threads_are_seen_snoozed_and_reordered_and_repos_take_icons() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let params = start_params(Some(repo.id), "Write some notes");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    assert_eq!(started.thread.seen_at, None);
    assert_eq!(
        started.thread.last_prompt_at,
        Some(started.thread.created_at)
    );
    let mut runs = host.client().await;
    runs.subscribe(0, Some(scope(repo.id))).await;
    runs.until(updated_to(AgentStatus::Completed)).await;
    let seq = client.list().await.seq;
    client.subscribe(seq, None).await;

    let until: jiff::Timestamp = "2030-01-01T09:00:00Z".parse().unwrap();
    let updated = client
        .call::<ThreadUpdate>(attention(params.run_id, true, Some(until)))
        .await
        .unwrap()
        .thread;
    assert!(updated.seen_at.unwrap() >= started.thread.created_at);
    assert_eq!(updated.snoozed_until, Some(until));
    client
        .until(|event| matches!(&event.event, ParallaxEvent::ThreadUpdated { thread } if *thread == updated))
        .await;
    // The same snooze again changes nothing.
    let again = client
        .call::<ThreadUpdate>(attention(params.run_id, false, Some(until)))
        .await
        .unwrap()
        .thread;
    assert_eq!(again, updated);

    client
        .call::<AgentSend>(message(params.run_id, "And a summary"))
        .await
        .unwrap();
    let prompted = client
        .until(|event| matches!(&event.event, ParallaxEvent::ThreadUpdated { thread } if thread.last_prompt_at > updated.last_prompt_at))
        .await;
    let ParallaxEvent::ThreadUpdated { thread } = &prompted.last().unwrap().event else {
        unreachable!()
    };
    assert_eq!(client.list().await.threads, std::slice::from_ref(thread));

    let icon = ProjectIcon {
        name: "flame".to_owned(),
        color: Some("orange".to_owned()),
        image: None,
    };
    let with_icon = client
        .call::<RepoUpdate>(RepoUpdateParams {
            repo: repo.id,
            icon: icon.clone(),
        })
        .await
        .unwrap()
        .repo;
    assert_eq!(with_icon.icon, Some(icon.clone()));
    client
        .until(|event| matches!(&event.event, ParallaxEvent::RepoUpdated { repo } if repo.icon.is_some()))
        .await;
    assert_eq!(client.list().await.repos, [with_icon]);

    let bad = client
        .call::<RepoUpdate>(RepoUpdateParams {
            repo: repo.id,
            icon: ProjectIcon {
                name: "Not An Icon".to_owned(),
                color: None,
                image: None,
            },
        })
        .await
        .unwrap_err();
    assert_eq!(bad.code, INVALID_PARAMS, "{bad:?}");
    let missing = client
        .call::<RepoUpdate>(RepoUpdateParams {
            repo: RepoId::generate(),
            icon,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&missing), ErrorKind::RepoNotFound);
    let missing = client
        .call::<ThreadUpdate>(attention(RunId::generate(), true, None))
        .await
        .unwrap_err();
    assert_eq!(kind(&missing), ErrorKind::ThreadNotFound);
}

/// Lineage (0041): a thread started with a parent and a title lists them, `thread/update` renames
/// and settles it with `thread.updated`, and deleting its parent leaves it with none.
#[tokio::test]
async fn a_child_thread_keeps_its_parent_and_title_until_the_parent_is_deleted() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let parent = start_params(Some(repo.id), "Plan the work");
    client.call::<ThreadStart>(parent.clone()).await.unwrap();

    let orphan = client
        .call::<ThreadStart>(ThreadStartParams {
            parent: Some(RunId::generate()),
            ..start_params(Some(repo.id), "Nobody launched this")
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&orphan), ErrorKind::RunNotFound);
    let child = ThreadStartParams {
        parent: Some(parent.run_id),
        title: Some("  Write the tests ".to_owned()),
        ..start_params(Some(repo.id), "Write the tests")
    };
    let started = client
        .call::<ThreadStart>(child.clone())
        .await
        .unwrap()
        .thread;
    assert_eq!(started.parent, Some(parent.run_id));
    assert_eq!(started.title.as_deref(), Some("Write the tests"));
    assert!(!started.settled);
    let reparented = client
        .call::<ThreadStart>(ThreadStartParams {
            parent: None,
            ..child.clone()
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&reparented), ErrorKind::IdConflict);

    let seq = client.list().await.seq;
    client.subscribe(seq, None).await;
    let update = |title: &str, settled| ThreadUpdateParams {
        run_id: child.run_id,
        seen: false,
        snoozed_until: None,
        title: Some(title.to_owned()),
        settled,
    };
    let updated = client
        .call::<ThreadUpdate>(update("Test attach", Some(true)))
        .await
        .unwrap()
        .thread;
    assert_eq!(updated.title.as_deref(), Some("Test attach"));
    assert!(updated.settled);
    client
        .until(|event| matches!(&event.event, ParallaxEvent::ThreadUpdated { thread } if *thread == updated))
        .await;
    let long = "t".repeat(parallax_protocol::MAX_THREAD_TITLE_BYTES + 1);
    let too_long = client
        .call::<ThreadUpdate>(update(&long, None))
        .await
        .unwrap_err();
    assert_eq!(too_long.code, INVALID_PARAMS, "{too_long:?}");

    client.delete(parent.run_id).await.unwrap();
    client
        .until(|event| matches!(&event.event, ParallaxEvent::ThreadUpdated { thread } if thread.id == child.run_id && thread.parent.is_none()))
        .await;
    let threads = client.list().await.threads;
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].parent, None);
    assert_eq!(threads[0].title.as_deref(), Some("Test attach"));

    let cleared = client
        .call::<ThreadUpdate>(update("", Some(false)))
        .await
        .unwrap()
        .thread;
    assert_eq!(cleared.title, None);
    assert!(!cleared.settled);
}

/// A `thread/start` retried after its parent was deleted returns the thread, now with no parent,
/// rather than `idConflict` (0041).
#[tokio::test]
async fn a_retried_start_whose_parent_was_deleted_returns_the_thread() {
    let host = Host::start(fake(editing()));
    let path = real_repo(host.work.path(), "app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let parent = start_params(Some(repo.id), "Plan the work");
    client.call::<ThreadStart>(parent.clone()).await.unwrap();
    let child = ThreadStartParams {
        parent: Some(parent.run_id),
        ..start_params(Some(repo.id), "Write the tests")
    };
    client.call::<ThreadStart>(child.clone()).await.unwrap();

    client.delete(parent.run_id).await.unwrap();
    let retried = client.call::<ThreadStart>(child.clone()).await.unwrap();
    assert_eq!(retried.thread.id, child.run_id);
    assert_eq!(retried.thread.parent, None);
}
