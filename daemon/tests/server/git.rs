//! The Git menu's methods end to end (RYA-298): `agent/gitStatus`, `agent/commit`, and
//! `agent/push` in a run's worktree and in a Current checkout thread's checkout, pushing to a
//! local bare `origin`, with `open_pr`'s fake `gh` for Create PR.

use std::fs;
use std::path::PathBuf;

use parallax_protocol::methods::{
    AgentCancel, AgentCommit, AgentGitStatus, AgentOpenPr, AgentPush, AgentStart, RepoAdd,
    ThreadStart,
};
use parallax_protocol::{
    AgentCancelParams, AgentCommitParams, AgentGitStatusParams, AgentPushParams, AgentStatus,
    ErrorKind, GitStatus, ParallaxEvent, ProjectId, RepoAddParams, RepoId, RunId,
    ThreadStartParams,
};
use plxd::backend::fake::Step;

use crate::agents::{
    Conn, create, git, init, project_params, real_repo, start_params, subscribe, text, until,
    updated_to,
};
use crate::open_pr::{Tools, add_origin, editing, open, start, thread};
use crate::support::{kind, temp_dir};

async fn status(client: &mut Conn, run_id: RunId) -> GitStatus {
    client
        .call::<AgentGitStatus>(AgentGitStatusParams { run_id })
        .await
        .unwrap()
}

fn commit(run_id: RunId, message: &str) -> AgentCommitParams {
    AgentCommitParams {
        run_id,
        message: message.to_owned(),
    }
}

#[tokio::test]
async fn a_worktree_run_commits_the_user_s_edits_and_pushes_its_branch() {
    let tools = Tools::new();
    let dir = temp_dir();
    let params = project_params(dir.path());
    let repo = PathBuf::from(&params.repo_path);
    let origin = add_origin(&repo, dir.path());
    let host = start(dir, editing(), &tools);
    let mut client = host.client().await;
    let project = create(&mut client, params).await;
    subscribe(&mut client, project.id, 0).await;
    let start = start_params(project.id, "Rewrite the README");
    let run_id = start.run_id;
    let run = client.call::<AgentStart>(start).await.unwrap().run;
    let branch = run.branch.unwrap();
    let worktree = PathBuf::from(run.worktree_path.unwrap());
    until(&mut client, updated_to(AgentStatus::Completed)).await;

    // The turn's commit, and the repository's first, are on none of origin's branches.
    assert_eq!(
        status(&mut client, run_id).await,
        GitStatus {
            branch: Some(branch.clone()),
            changes: 0,
            upstream: None,
            ahead: 2,
            origin: true,
        }
    );

    fs::write(worktree.join("NOTES.md"), "notes\n").unwrap();
    assert_eq!(status(&mut client, run_id).await.changes, 1);
    let committed = client
        .call::<AgentCommit>(commit(run_id, "Add notes"))
        .await
        .unwrap();
    assert_eq!((committed.changes, committed.ahead), (0, 3));
    assert_eq!(git(&worktree, &["log", "-1", "--format=%s"]), "Add notes");
    // The commit is the run's now, for Accept and Open PR.
    let head = git(&worktree, &["rev-parse", "HEAD"]);
    until(
        &mut client,
        |e| matches!(&e.event, ParallaxEvent::AgentDiffReady { diff, .. } if diff.commit == head),
    )
    .await;

    let nothing = client
        .call::<AgentCommit>(commit(run_id, "Again"))
        .await
        .unwrap_err();
    assert_eq!(kind(&nothing), ErrorKind::GitRefused);
    assert!(
        nothing.message.contains("no changes"),
        "{}",
        nothing.message
    );
    let blank = client
        .call::<AgentCommit>(commit(run_id, " \n "))
        .await
        .unwrap_err();
    assert_eq!(blank.code, parallax_protocol::jsonrpc::INVALID_PARAMS);

    let pushed = client
        .call::<AgentPush>(AgentPushParams { run_id })
        .await
        .unwrap();
    assert_eq!(pushed.upstream, Some(format!("origin/{branch}")));
    assert_eq!(pushed.ahead, 0);
    assert_eq!(git(&origin, &["rev-parse", &branch]), head);

    // Ahead of the upstream now.
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", "More"]);
    assert_eq!(status(&mut client, run_id).await.ahead, 1);
    host.server.stop().await;
}

#[tokio::test]
async fn a_checkout_thread_commits_pushes_and_opens_a_pull_request_on_its_branch() {
    let tools = Tools::new();
    let work = temp_dir();
    let repo = real_repo(work.path());
    let origin = add_origin(&repo, work.path());
    let host = start(temp_dir(), editing(), &tools);
    let mut client = host.client().await;
    let entry = client
        .call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: repo.to_str().unwrap().to_owned(),
        })
        .await
        .unwrap()
        .repo;
    subscribe(
        &mut client,
        ProjectId::try_from(uuid::Uuid::from(entry.id)).unwrap(),
        0,
    )
    .await;
    let start = ThreadStartParams {
        checkout: true,
        ..thread(Some(entry.id))
    };
    let run_id = start.run_id;
    client.call::<ThreadStart>(start).await.unwrap();
    until(&mut client, updated_to(AgentStatus::Completed)).await;

    // The thread rewrote README.md in the checkout, and a checkout is never committed for it.
    assert_eq!(
        status(&mut client, run_id).await,
        GitStatus {
            branch: Some("main".to_owned()),
            changes: 1,
            upstream: None,
            ahead: 1,
            origin: true,
        }
    );
    let committed = client
        .call::<AgentCommit>(commit(run_id, "Rewrite the README"))
        .await
        .unwrap();
    assert_eq!((committed.changes, committed.ahead), (0, 2));
    assert_eq!(
        git(&repo, &["log", "-1", "--format=%s %an"]),
        "Rewrite the README Test User"
    );

    let pushed = client
        .call::<AgentPush>(AgentPushParams { run_id })
        .await
        .unwrap();
    assert_eq!(
        (pushed.upstream.as_deref(), pushed.ahead),
        (Some("origin/main"), 0)
    );
    assert_eq!(
        git(&origin, &["rev-parse", "main"]),
        git(&repo, &["rev-parse", "HEAD"])
    );

    // Create PR pushes the branch the checkout has out.
    let opened = client
        .call::<AgentOpenPr>(open(run_id, "Rewrite the README", None))
        .await
        .unwrap();
    assert!(opened.url.ends_with("/pull/7"), "{}", opened.url);
    assert!(tools.log()[0].contains("--head=main"), "{:?}", tools.log());

    // A detached HEAD has no branch to push or open a pull request from.
    git(&repo, &["checkout", "-q", "--detach"]);
    assert_eq!(status(&mut client, run_id).await.branch, None);
    let push = client
        .call::<AgentPush>(AgentPushParams { run_id })
        .await
        .unwrap_err();
    assert_eq!(kind(&push), ErrorKind::GitRefused);
    assert!(push.message.contains("detached HEAD"), "{}", push.message);
    let pr = client
        .call::<AgentOpenPr>(open(run_id, "Rewrite the README", None))
        .await
        .unwrap_err();
    assert_eq!(kind(&pr), ErrorKind::PrRefused);
    assert!(pr.message.contains("detached HEAD"), "{}", pr.message);
    host.server.stop().await;
}

#[tokio::test]
async fn a_running_run_reads_its_status_but_refuses_commit_and_push() {
    let tools = Tools::new();
    let dir = temp_dir();
    let params = project_params(dir.path());
    let host = start(dir, vec![init("s"), text("Working"), Step::Hang], &tools);
    let mut client = host.client().await;
    let project = create(&mut client, params).await;
    subscribe(&mut client, project.id, 0).await;
    let start = start_params(project.id, "Work forever");
    let run_id = start.run_id;
    client.call::<AgentStart>(start).await.unwrap();
    until(&mut client, updated_to(AgentStatus::Running)).await;

    assert!(!status(&mut client, run_id).await.origin);
    let commit = client
        .call::<AgentCommit>(commit(run_id, "Work"))
        .await
        .unwrap_err();
    let push = client
        .call::<AgentPush>(AgentPushParams { run_id })
        .await
        .unwrap_err();
    for error in [commit, push] {
        assert_eq!(kind(&error), ErrorKind::GitRefused);
        assert!(error.message.contains("still running"), "{}", error.message);
    }
    client
        .call::<AgentCancel>(AgentCancelParams { run_id, from: None })
        .await
        .unwrap();
    until(&mut client, updated_to(AgentStatus::Cancelled)).await;
    host.server.stop().await;
}
