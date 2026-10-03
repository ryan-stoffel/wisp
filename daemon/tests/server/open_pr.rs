//! `agent/openPr` end to end (RYA-168): a run's branch goes to a local bare `origin`, and a fake
//! `gh` stands in for GitHub. plxd's `PATH` holds only git and that fake, so no test can reach
//! the real GitHub.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use parallax_protocol::methods::{
    AgentCancel, AgentList, AgentOpenPr, AgentStart, RepoAdd, ThreadStart,
};
use parallax_protocol::{
    AccountChoice, AgentCancelParams, AgentListParams, AgentOpenPrParams, AgentStatus, ErrorKind,
    ParallaxEvent, ProjectId, RepoAddParams, RepoId, RunId, ThreadStartParams,
};
use plxd::backend::fake::Step;
use plxd::backend::process::{Environment, find_program};
use tempfile::TempDir;

use crate::agents::{
    Conn, Host, create, end_turn, fake, git, init, project_params, real_repo, start_params,
    subscribe, text, until, updated_to,
};
use crate::support::{InProcess, kind, temp_dir};

const URL: &str = "https://github.com/example/app/pull/7";

/// A folder that is plxd's whole `PATH`: the real git, and a fake `gh` that logs its arguments to
/// `gh.log`, answers `pr list` with a fork's pull request from a branch of the same name, then the
/// one `pr create` made, answers `pr view` with what [`Tools::view`] wrote, `pr diff` with a
/// one-line diff, succeeds at `pr merge`,
/// `pr ready`, and `pr close`, and fails as `gh-mode` says: `signed-out` (exit 4, as gh does) or
/// `fail`.
pub(crate) struct Tools(TempDir);

impl Tools {
    pub(crate) fn new() -> Self {
        let tools = Self(temp_dir());
        let bin = tools.0.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let git = find_program("git".as_ref(), std::env::var_os("PATH").as_deref()).unwrap();
        let git = format!("#!/bin/sh\nexec '{}' \"$@\"\n", git.display());
        let gh = format!(
            r#"#!/bin/sh
# Builtins only: nothing else is on the PATH.
dir='{dir}'
printf '%s\n' "$*" >> "$dir/gh.log"
mode=; [ -f "$dir/gh-mode" ] && read -r mode < "$dir/gh-mode"
case "$mode" in
  signed-out) echo 'To get started with GitHub CLI, please run:  gh auth login' >&2; exit 4 ;;
  fail) echo 'GraphQL: Could not resolve to a Repository' >&2; exit 1 ;;
esac
case "$1 $2" in
  'pr list')
    list='{{"url":"https://github.com/someone/app/pull/3","isCrossRepository":true}}'
    if [ -f "$dir/pr" ]; then
      read -r url < "$dir/pr"
      list="$list,{{\"url\":\"$url\",\"isCrossRepository\":false}}"
    fi
    echo "[$list]" ;;
  'pr create') echo '{URL}' > "$dir/pr"; echo 'Creating pull request' >&2; echo '{URL}' ;;
  'pr view') while IFS= read -r line; do printf '%s\n' "$line"; done < "$dir/view.json" ;;
  'pr diff') echo 'diff --git a/README.md b/README.md' ;;
  'pr merge' | 'pr ready' | 'pr close') ;;
  *) exit 1 ;;
esac
"#,
            dir = tools.0.path().display()
        );
        for (name, script) in [("git", git), ("gh", gh)] {
            fs::write(bin.join(name), script).unwrap();
            fs::set_permissions(bin.join(name), fs::Permissions::from_mode(0o755)).unwrap();
        }
        tools
    }

    fn environment(&self) -> Environment {
        let mut env = Environment::inherited();
        env.set("PATH", self.0.path().join("bin"));
        env
    }

    pub(crate) fn log(&self) -> Vec<String> {
        fs::read_to_string(self.0.path().join("gh.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// What `gh pr view` prints.
    pub(crate) fn view(&self, json: &str) {
        fs::write(self.0.path().join("view.json"), format!("{json}\n")).unwrap();
    }

    pub(crate) fn mode(&self, mode: &str) {
        fs::write(self.0.path().join("gh-mode"), mode).unwrap();
    }

    /// Takes `gh` off the `PATH`, or puts it back.
    fn installed(&self, installed: bool) {
        let (on, off) = (
            self.0.path().join("bin/gh"),
            self.0.path().join("gh.uninstalled"),
        );
        if installed {
            fs::rename(off, on).unwrap();
        } else {
            fs::rename(on, off).unwrap();
        }
    }
}

pub(crate) fn start(dir: TempDir, steps: Vec<Step>, tools: &Tools) -> Host {
    let mut config = InProcess::config(dir.path());
    config.backends = Some(fake(steps));
    config.agent_environment = Some(tools.environment());
    Host {
        dir,
        server: InProcess::start(config),
    }
}

pub(crate) fn editing() -> Vec<Step> {
    vec![
        init("s"),
        Step::WriteFile {
            path: "README.md".to_owned(),
            content: "# App\n".to_owned(),
        },
        end_turn("Done."),
    ]
}

/// A bare repository under `dir`, added to `repo` as its `origin`.
pub(crate) fn add_origin(repo: &Path, dir: &Path) -> PathBuf {
    let origin = dir.join("origin.git");
    git(dir, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    git(repo, &["remote", "add", "origin", origin.to_str().unwrap()]);
    origin
}

pub(crate) fn thread(repo: Option<RepoId>) -> ThreadStartParams {
    ThreadStartParams {
        run_id: RunId::generate(),
        repo,
        parent: None,
        title: None,
        prompt: "Rewrite the README".to_owned(),
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

pub(crate) fn open(run_id: RunId, title: &str, body: Option<&str>) -> AgentOpenPrParams {
    AgentOpenPrParams {
        run_id,
        title: title.to_owned(),
        body: body.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_finished_run_pushes_its_branch_and_opens_one_pull_request() {
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
    until(&mut client, updated_to(AgentStatus::Completed)).await;

    let opened = client
        .call::<AgentOpenPr>(open(
            run_id,
            "  Rewrite the README\nwith more",
            Some("Built in Parallax."),
        ))
        .await
        .unwrap();
    assert_eq!(opened.url, URL);
    let pushed = || git(&origin, &["rev-parse", &branch]);
    assert_eq!(pushed(), git(&repo, &["rev-parse", &branch]));
    let origin = origin.to_str().unwrap();
    assert_eq!(
        tools.log(),
        [
            format!(
                "pr list --repo={origin} --head={branch} --state=open \
                 --json=url,isCrossRepository"
            ),
            format!(
                "pr create --repo={origin} --head={branch} --title=Rewrite the README \
                 --body=Built in Parallax."
            ),
        ]
    );

    // Opening it again pushes a new commit and finds the same pull request.
    let worktree = PathBuf::from(run.worktree_path.unwrap());
    git(&worktree, &["commit", "-q", "--allow-empty", "-m", "More"]);
    let again = client
        .call::<AgentOpenPr>(open(run_id, "Rewrite the README", None))
        .await
        .unwrap();
    assert_eq!(again.url, URL);
    assert_eq!(pushed(), git(&worktree, &["rev-parse", "HEAD"]));
    assert_eq!(tools.log().len(), 3, "a list, and no second create");

    // The run links its pull request once (PLX-318).
    let runs = client
        .call::<AgentList>(AgentListParams::default())
        .await
        .unwrap()
        .runs;
    assert_eq!(runs[0].pull_requests, [URL]);
    host.server.stop().await;
}

#[tokio::test]
async fn a_thread_s_pull_request_says_what_is_missing() {
    let tools = Tools::new();
    let work = temp_dir();
    let repo = real_repo(work.path());
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
    let start = thread(Some(entry.id));
    let run_id = start.run_id;
    client.call::<ThreadStart>(start).await.unwrap();
    until(&mut client, updated_to(AgentStatus::Completed)).await;
    let refused = async |client: &mut Conn, expected: ErrorKind, says: &str| {
        let error = client
            .call::<AgentOpenPr>(open(run_id, "Rewrite the README", None))
            .await
            .unwrap_err();
        assert_eq!(kind(&error), expected, "{}", error.message);
        assert!(error.message.contains(says), "{}", error.message);
    };
    refused(
        &mut client,
        ErrorKind::PushFailed,
        "No such remote 'origin'",
    )
    .await;
    add_origin(&repo, work.path());
    tools.installed(false);
    refused(&mut client, ErrorKind::GhUnavailable, "isn't installed").await;
    tools.installed(true);
    tools.mode("signed-out");
    refused(&mut client, ErrorKind::GhUnavailable, "gh auth login").await;
    tools.mode("fail");
    refused(
        &mut client,
        ErrorKind::PrFailed,
        "Could not resolve to a Repository",
    )
    .await;
    tools.mode("");
    let opened = client
        .call::<AgentOpenPr>(open(run_id, "Rewrite the README", None))
        .await
        .unwrap();
    assert_eq!(opened.url, URL);

    // A thread with no repo has only its scratch repository, with nowhere to push.
    let start = thread(None);
    let scratch = start.run_id;
    let started = client.call::<ThreadStart>(start).await.unwrap();
    let scope = ProjectId::try_from(uuid::Uuid::from(started.thread.repo)).unwrap();
    subscribe(&mut client, scope, 0).await;
    // Not the first thread's `agent.updated` for the pull request it linked.
    until(&mut client, |event| {
        matches!(&event.event, ParallaxEvent::AgentUpdated { run_id, state }
            if *run_id == scratch && state.status == AgentStatus::Completed)
    })
    .await;
    let error = client
        .call::<AgentOpenPr>(open(scratch, "Rewrite the README", None))
        .await
        .unwrap_err();
    assert_eq!(kind(&error), ErrorKind::PrRefused);
    assert!(error.message.contains("no repository"), "{}", error.message);
    host.server.stop().await;
}

#[tokio::test]
async fn a_running_run_or_one_with_nothing_committed_is_refused() {
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

    let running = client
        .call::<AgentOpenPr>(open(run_id, "Work", None))
        .await
        .unwrap_err();
    assert_eq!(kind(&running), ErrorKind::PrRefused);
    assert!(
        running.message.contains("still running"),
        "{}",
        running.message
    );

    client
        .call::<AgentCancel>(AgentCancelParams { run_id, from: None })
        .await
        .unwrap();
    until(&mut client, updated_to(AgentStatus::Cancelled)).await;
    let empty = client
        .call::<AgentOpenPr>(open(run_id, "Work", None))
        .await
        .unwrap_err();
    assert_eq!(kind(&empty), ErrorKind::PrRefused);
    assert!(
        empty.message.contains("no committed changes"),
        "{}",
        empty.message
    );

    let blank = client
        .call::<AgentOpenPr>(open(run_id, " \n ", None))
        .await
        .unwrap_err();
    assert_eq!(blank.code, parallax_protocol::jsonrpc::INVALID_PARAMS);
    assert!(tools.log().is_empty(), "gh never ran");
    host.server.stop().await;
}
