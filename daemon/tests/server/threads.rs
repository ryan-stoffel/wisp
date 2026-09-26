//! Normal threads end to end (#110): `thread/*` and `repo/*` against an in-process server whose
//! worker backend is the fake CLI, in real git repositories.

use std::path::{Path, PathBuf};

use tempfile::TempDir;
use wisp_protocol::jsonrpc::{ErrorObject, INVALID_PARAMS};
use wisp_protocol::methods::{
    AgentAccept, AgentEvents, AgentList, AgentSend, HostHealth, RepoAdd, ThreadArchive,
    ThreadDelete, ThreadList, ThreadStart,
};
use wisp_protocol::{
    AcceptId, AccountChoice, AgentAcceptParams, AgentEventsParams, AgentListParams,
    AgentSendParams, AgentStatus, ErrorKind, HostHealthParams, ProjectId, Repo, RepoAddParams,
    RepoId, RunId, ThreadArchiveParams, ThreadDeleteParams, ThreadListParams, ThreadListResult,
    ThreadStartParams, TurnId, WispEvent,
};
use wispd::backend::fake::Step;
use wispd::routing::BackendRegistry;

use crate::support::{
    Conn, InProcess, end_turn, fake, git, init, kind, real_repo, temp_dir, text, updated_to,
};

fn editing() -> Vec<Step> {
    vec![
        init("thread-1"),
        Step::WriteFile {
            path: "NOTES.md".to_owned(),
            content: "Written in a thread.\n".to_owned(),
        },
        text("Done."),
        end_turn("Done."),
    ]
}

fn hang() -> Vec<Step> {
    vec![init("hang-1"), Step::Hang]
}

fn start_params(repo: Option<RepoId>, prompt: &str) -> ThreadStartParams {
    ThreadStartParams {
        run_id: RunId::generate(),
        repo,
        prompt: prompt.to_owned(),
        account: Some(AccountChoice::Subscription {
            backend: "fake".to_owned(),
        }),
    }
}

struct Host {
    /// wispd's data folder.
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
        Conn::ready(&self.server.socket).await
    }

    fn data(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    /// A new repository `name` among the tests' own.
    fn repo(&self, name: &str) -> PathBuf {
        real_repo(self.work.path(), name).canonicalize().unwrap()
    }
}

impl Conn {
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

fn scope(repo: RepoId) -> ProjectId {
    ProjectId::try_from(uuid::Uuid::from(repo)).unwrap()
}

#[tokio::test]
async fn a_thread_runs_in_a_worktree_of_its_repo_entry_and_lists_under_it() {
    let host = Host::start(fake(editing()));
    let path = host.repo("app");
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
        .until(|event| matches!(event.event, WispEvent::RepoAdded { .. }))
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
        .until(|event| matches!(&event.event, WispEvent::ThreadStarted { thread } if thread.id == params.run_id))
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
            .any(|event| matches!(event.event, WispEvent::AgentDiffReady { .. }))
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
async fn a_thread_with_no_repo_gets_its_own_scratch_repository() {
    let host = Host::start(fake(editing()));
    let mut client = host.client().await;
    client.subscribe(0, None).await;

    let params = start_params(None, "Jot something down");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let events = client
        .until(|event| matches!(event.event, WispEvent::ThreadStarted { .. }))
        .await;
    let scratch = events
        .iter()
        .find_map(|event| match &event.event {
            WispEvent::RepoAdded { repo } => Some(repo.clone()),
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
        "wisp wisp: Jot something down\nwisp Start a wisp scratch folder"
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
            |event| matches!(&event.event, WispEvent::ThreadUpdated { thread } if thread.archived),
        )
        .await;
    assert!(client.list().await.threads[0].archived);

    client.delete(params.run_id).await.unwrap();
    client
        .until(|event| matches!(&event.event, WispEvent::ThreadDeleted { run_id, .. } if *run_id == params.run_id))
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
    let path = host.repo("app");
    let mut client = host.client().await;
    let repo = client.add(&path).await;
    let params = start_params(Some(repo.id), "Write some notes");
    let started = client.call::<ThreadStart>(params.clone()).await.unwrap();
    let worktree = PathBuf::from(started.run.worktree_path.unwrap());
    let branch = started.run.branch.unwrap();
    client.subscribe(0, Some(scope(repo.id))).await;
    client.until(updated_to(AgentStatus::Completed)).await;

    let send = client
        .send::<AgentSend>(AgentSendParams {
            run_id: params.run_id,
            turn_id: TurnId::generate(),
            text: "And some more".to_owned(),
        })
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
        .send::<AgentSend>(AgentSendParams {
            run_id: params.run_id,
            turn_id: TurnId::generate(),
            text: "Still there?".to_owned(),
        })
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
    let path = host.repo("app");
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

    let path = host.repo("app");
    let other = host.repo("other");
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
