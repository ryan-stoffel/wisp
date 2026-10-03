//! Real Claude Code for RYA-222 (0031): in Manual, a run whose client answers permission requests
//! asks plxd over stdio before a tool call that would prompt, and runs it or not on plxd's
//! answer, and a run whose client doesn't is denied as before. In Plan, a worker hands its plan
//! over the same way (RYA-243). A worker also keeps its plan with Claude Code's task tools, which
//! ask nothing (RYA-248), and so do a coordinator and a bypass worker, whose `--allowedTools`
//! turns them on (RYA-249). Each keeps its session's own list, whatever the settings it reads
//! name in `CLAUDE_CODE_TASK_LIST_ID` (RYA-251). A thread, full Claude Code, asks before a `git
//! commit` in Accept Edits, and its commit lands once allowed (RYA-276). Each test starts the CLI
//! through plxd's own Claude backend, so the arguments, the translator that reads the CLI's
//! `can_use_tool` request, and the driver that writes the `control_response` are the ones a real
//! run uses. A local fake Messages API asks for the tool calls, so no account or Anthropic
//! connection is needed. Set `PLX_SANDBOX_CLAUDE` to the CLI under test, as CI's Linux legs do.
#![cfg(unix)]

#[expect(
    dead_code,
    reason = "these tests run the CLI through plxd's backend, not `run_worker`"
)]
mod common;

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use common::{ToolCall, fake_api, worker_request};
use parallax_protocol::{CoordinatorThreadId, ProjectId};
use plxd::backend::claude::ClaudeBackend;
use plxd::backend::process::{Environment, Launcher};
use plxd::backend::run_temp::RunTemp;
use plxd::backend::{
    AccountRef, AgentPermission, Answer, ApiKey, ApprovalRequest, Backend, CoordinatorTools,
    Credential, Decision, Event, Outcome, RunId, RunRequest, Started, ToolPolicy, ToolStatus,
};
use plxd::paths::DataDir;
use serde_json::{Value, json};

const KEY: &str = "sk-ant-parallax-test-key-never-send";

/// What the fake API's Bash calls run: it says so, and leaves a line in `ran` for each run.
const PROBE: &str = "echo probe-ran\necho x >> ran\n";

/// The task list [`share_the_task_list`] names, which no run may use (RYA-251).
const SHARED_LIST: &str = "parallax-shared-list";

/// Where [`WRAPPER`] finds the fake API's base URL.
const API_ENV: &str = "PLX_TEST_API_URL";
/// Where [`WRAPPER`] finds the CLI under test.
const CLAUDE_ENV: &str = "PLX_TEST_CLAUDE";

/// The program the backend starts: the CLI under test, against the fake API. plxd passes no
/// inherited `ANTHROPIC_` variable on to a run, so the base URL can only reach the CLI this way.
const WRAPPER: &str = "#!/bin/sh\n\
    export ANTHROPIC_BASE_URL=\"$PLX_TEST_API_URL\" DISABLE_AUTOUPDATER=1 \
    CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1\n\
    exec \"$PLX_TEST_CLAUDE\" \"$@\"\n";

/// [`WRAPPER`], written once, before any test here starts a process, so no other test's child
/// can still hold it open for writing when it runs ("Text file busy").
fn wrapper() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let path = Path::new(env!("CARGO_TARGET_TMPDIR")).join("claude-against-fake-api");
        fs::write(&path, WRAPPER).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    })
}

/// plxd's Claude backend, as `serve` builds it, with `data` as plxd's data folder and `home` as
/// `HOME`, starting `claude` against the fake API at `api`.
fn claude_backend(
    claude: &OsStr,
    api: &str,
    root: &Path,
    home: &Path,
    data: &Path,
) -> ClaudeBackend {
    // The CLI's own `TMPDIR`, plxd's in a real run.
    fs::create_dir_all(root.join("tmp")).unwrap();
    let mut env = Environment::empty();
    env.set("PATH", std::env::var_os("PATH").unwrap_or_default());
    env.set("HOME", home);
    env.set("TMPDIR", root.join("tmp"));
    env.set(API_ENV, api);
    env.set(CLAUDE_ENV, claude);
    ClaudeBackend::new(Launcher::new(DataDir::new(data).unwrap(), env)).with_program(wrapper())
}

/// A coordinator in Manual at `cwd`, which is full Claude Code (0027), on an API key, whose
/// client answers permission requests. Its plxd tools' server can't reach a plxd, so it fails
/// to start, which the run doesn't need.
fn coordinator(cwd: &Path, data: &Path) -> RunRequest {
    RunRequest {
        run_id: RunId::generate(),
        turn_id: None,
        cwd: cwd.to_owned(),
        prompt: "Run the probe.".into(),
        images: Vec::new(),
        policy: ToolPolicy::NoWrite,
        sandbox: None,
        account: AccountRef {
            id: "test".into(),
            credential: Credential::ApiKey(ApiKey::new(KEY.into())),
        },
        resume: None,
        model: Some("claude-sonnet-4-6".into()),
        effort: None,
        permission: Some(AgentPermission::Manual),
        context_window: None,
        fast: None,
        coordinator_tools: Some(CoordinatorTools {
            program: PathBuf::from(env!("CARGO_BIN_EXE_plxd")),
            data_dir: data.to_owned(),
            project: ProjectId::generate(),
            thread: CoordinatorThreadId::generate(),
        }),
        thread_tools: None,
        approvals: true,
        thread: false,
    }
}

/// A folder for one test, with `home`, `data`, and a `project` folder that holds [`PROBE`].
struct Folders {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
    project: PathBuf,
}

impl Folders {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (home, data, project) = (root.join("home"), root.join("data"), root.join("project"));
        for folder in [&home, &data, &project] {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(project.join("probe.sh"), PROBE).unwrap();
        Self {
            _dir: dir,
            root,
            home,
            data,
            project,
        }
    }
}

/// Runs `request` to its end, answering each permission request with `decide`'s decision, and
/// returns its events.
async fn drive(
    backend: &ClaudeBackend,
    request: RunRequest,
    decide: impl Fn(&ApprovalRequest) -> Decision,
) -> Vec<Event> {
    let Started { run, mut events } = backend.start(request).unwrap();
    let mut all = Vec::new();
    loop {
        let Ok(event) = tokio::time::timeout(Duration::from_secs(120), events.next()).await else {
            panic!("no event within 120 s, after {all:#?}");
        };
        let event = event.expect("the stream ended before Finished");
        if let Event::ApprovalRequested(asked) = &event {
            run.answer(Answer {
                approval_id: asked.approval_id,
                decision: decide(asked),
            })
            .unwrap();
        }
        let last = event.is_terminal();
        all.push(event);
        if last {
            return all;
        }
    }
}

fn requests(events: &[Event]) -> Vec<&ApprovalRequest> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::ApprovalRequested(asked) => Some(asked),
            _ => None,
        })
        .collect()
}

/// Each tool result's call id, status, and output.
fn results(events: &[Event]) -> Vec<(&str, ToolStatus, &str)> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult {
                call_id,
                status,
                output,
            } => Some((call_id.as_str(), *status, output.as_deref().unwrap_or(""))),
            _ => None,
        })
        .collect()
}

fn outcome(events: &[Event]) -> &Outcome {
    match events.last() {
        Some(Event::Finished { outcome, .. }) => outcome,
        other => panic!("expected Finished last, got {other:?}"),
    }
}

fn done() -> Outcome {
    Outcome::Completed {
        result: Some("done".into()),
    }
}

/// A coordinator's Bash asks first. plxd allows it for the rest of the session with the rule the
/// CLI offered, so the probe runs, and the same call later runs without asking.
#[tokio::test]
async fn a_manual_coordinator_runs_bash_once_plxd_allows_it() {
    let Some(claude) = std::env::var_os("PLX_SANDBOX_CLAUDE") else {
        eprintln!("skipped: set PLX_SANDBOX_CLAUDE to test the real Claude Code CLI");
        return;
    };
    let folders = Folders::new();
    let probe = ToolCall::bash("sh probe.sh");
    let api = fake_api(vec![probe.clone(), probe]).await;
    let backend = claude_backend(&claude, &api, &folders.root, &folders.home, &folders.data);
    let request = coordinator(&folders.project, &folders.data);

    let events = drive(&backend, request, |_| Decision::Allow {
        input: None,
        always: true,
    })
    .await;
    let asked = requests(&events);
    assert_eq!(asked.len(), 1, "only the first call asks: {events:#?}");
    assert_eq!(asked[0].tool_name, "Bash");
    assert_eq!(asked[0].input["command"], "sh probe.sh");
    assert_eq!(asked[0].call_id.as_deref(), Some("toolu_01ParallaxProbe"));
    assert!(!asked[0].always_allow.is_empty(), "{events:#?}");
    assert!(!asked[0].interactive);
    let results = results(&events);
    assert_eq!(results.len(), 2, "{events:#?}");
    for (_, status, output) in results {
        assert_eq!(status, ToolStatus::Ok, "{events:#?}");
        assert!(output.contains("probe-ran"), "{events:#?}");
    }
    assert_eq!(
        fs::read_to_string(folders.project.join("ran")).unwrap(),
        "x\nx\n"
    );
    assert_eq!(outcome(&events), &done(), "{events:#?}");
}

/// A coordinator's Bash asks, plxd denies it with the user's words, and the CLI skips the call
/// and tells the model why.
#[tokio::test]
async fn a_manual_coordinator_skips_bash_that_plxd_denies() {
    const DENIAL: &str = "Not now: Parallax's test denies this call.";
    let Some(claude) = std::env::var_os("PLX_SANDBOX_CLAUDE") else {
        eprintln!("skipped: set PLX_SANDBOX_CLAUDE to test the real Claude Code CLI");
        return;
    };
    let folders = Folders::new();
    let api = fake_api(vec![ToolCall::bash("sh probe.sh")]).await;
    let backend = claude_backend(&claude, &api, &folders.root, &folders.home, &folders.data);
    let request = coordinator(&folders.project, &folders.data);

    let events = drive(&backend, request, |_| Decision::Deny {
        message: DENIAL.into(),
        interrupt: false,
    })
    .await;
    let asked = requests(&events);
    assert_eq!(asked.len(), 1, "{events:#?}");
    assert_eq!(asked[0].tool_name, "Bash");
    assert_eq!(asked[0].call_id.as_deref(), Some("toolu_01ParallaxProbe"));
    let results = results(&events);
    assert_eq!(results.len(), 1, "{events:#?}");
    let (call_id, status, output) = results[0];
    assert_eq!(call_id, "toolu_01ParallaxProbe");
    assert_ne!(status, ToolStatus::Ok, "{events:#?}");
    assert!(output.contains(DENIAL), "{events:#?}");
    assert!(!folders.project.join("ran").exists(), "the probe never ran");
    assert_eq!(outcome(&events), &done(), "{events:#?}");
}

/// A coordinator whose client doesn't answer runs as before the prompt channel: Claude Code
/// denies its Bash without asking anyone, and the turn goes on.
#[tokio::test]
async fn a_manual_coordinator_without_approvals_is_denied_without_asking() {
    let Some(claude) = std::env::var_os("PLX_SANDBOX_CLAUDE") else {
        eprintln!("skipped: set PLX_SANDBOX_CLAUDE to test the real Claude Code CLI");
        return;
    };
    let folders = Folders::new();
    let api = fake_api(vec![ToolCall::bash("sh probe.sh")]).await;
    let backend = claude_backend(&claude, &api, &folders.root, &folders.home, &folders.data);
    let request = RunRequest {
        approvals: false,
        ..coordinator(&folders.project, &folders.data)
    };

    let events = drive(&backend, request, |asked| {
        panic!("a run without approvals asked: {asked:?}")
    })
    .await;
    assert!(requests(&events).is_empty(), "{events:#?}");
    let results = results(&events);
    assert_eq!(results.len(), 1, "{events:#?}");
    assert_ne!(results[0].1, ToolStatus::Ok, "{events:#?}");
    assert!(!folders.project.join("ran").exists(), "the probe never ran");
    assert_eq!(outcome(&events), &done(), "{events:#?}");
}

/// A worker's worktree in `folders`' data folder, laid out as plxd lays one out and holding
/// [`PROBE`], and a request for a worker there in `permission`, on an API key, whose client
/// answers permission requests. Keep the [`RunTemp`] until the run is over.
fn worker(folders: &Folders, permission: AgentPermission) -> (PathBuf, RunRequest, RunTemp) {
    let worktree = folders.data.join("worktrees/run");
    let context = folders.data.join("context/p");
    let git_dir = folders.root.join("repo/.git");
    for folder in [&worktree, &context, &git_dir] {
        fs::create_dir_all(folder).unwrap();
    }
    fs::write(
        worktree.join(".git"),
        format!("gitdir: {}/worktrees/run\n", git_dir.display()),
    )
    .unwrap();
    fs::write(worktree.join("probe.sh"), PROBE).unwrap();
    let (mut request, temp) =
        worker_request(&folders.home, &folders.data, &worktree, &git_dir, &context);
    request.permission = Some(permission);
    request.approvals = true;
    request.account.credential = Credential::ApiKey(ApiKey::new(KEY.into()));
    (worktree, request, temp)
}

/// A worker in Manual, in its sandbox: its Bash runs without asking, since its settings allow
/// Bash (0013), and its `Write` asks. plxd allows it, and the file is written.
#[tokio::test]
async fn a_manual_worker_asks_before_writing_but_not_before_sandboxed_bash() {
    let Some(claude) = std::env::var_os("PLX_SANDBOX_CLAUDE") else {
        eprintln!("skipped: set PLX_SANDBOX_CLAUDE to test the real Claude Code CLI");
        return;
    };
    let folders = Folders::new();
    let (worktree, request, _temp) = worker(&folders, AgentPermission::Manual);
    let file = worktree.join("approved.txt");
    let write = ToolCall {
        name: "Write",
        input: json!({"file_path": file.to_str().unwrap(), "content": "approved\n"}),
    };
    let api = fake_api(vec![ToolCall::bash("sh probe.sh"), write]).await;
    let backend = claude_backend(&claude, &api, &folders.root, &folders.home, &folders.data);

    let events = drive(&backend, request, |_| Decision::Allow {
        input: None,
        always: false,
    })
    .await;
    let asked = requests(&events);
    assert_eq!(asked.len(), 1, "only the Write asks: {events:#?}");
    assert_eq!(asked[0].tool_name, "Write");
    assert_eq!(asked[0].input["file_path"], file.to_str().unwrap());
    assert_eq!(asked[0].call_id.as_deref(), Some("toolu_01ParallaxProbe1"));
    let results = results(&events);
    assert_eq!(results.len(), 2, "{events:#?}");
    assert_eq!(results[0].1, ToolStatus::Ok, "{events:#?}");
    assert!(results[0].2.contains("probe-ran"), "{events:#?}");
    assert_eq!(results[1].1, ToolStatus::Ok, "{events:#?}");
    assert_eq!(fs::read_to_string(&file).unwrap(), "approved\n");
    assert_eq!(outcome(&events), &done(), "{events:#?}");
}

/// A thread in Accept Edits is full Claude Code (RYA-276, 0034): its Bash isn't sandboxed, so a
/// `git commit` asks plxd first, and once allowed it writes the repository's git folder, which a
/// sandboxed worker's commands can't.
#[tokio::test]
async fn a_thread_commits_in_its_worktree_once_plxd_allows_it() {
    let Some(claude) = std::env::var_os("PLX_SANDBOX_CLAUDE") else {
        eprintln!("skipped: set PLX_SANDBOX_CLAUDE to test the real Claude Code CLI");
        return;
    };
    let folders = Folders::new();
    let repo = folders.root.join("repo");
    let worktree = folders.data.join("worktrees/run");
    let git = |dir: &Path, args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "parallax/run",
            worktree.to_str().unwrap(),
        ],
    );
    let context = folders.data.join("context/p");
    fs::create_dir_all(&context).unwrap();
    let (mut request, _temp) = worker_request(
        &folders.home,
        &folders.data,
        &worktree,
        &repo.join(".git"),
        &context,
    );
    request.thread = true;
    request.approvals = true;
    request.permission = Some(AgentPermission::Edit);
    request.account.credential = Credential::ApiKey(ApiKey::new(KEY.into()));
    let commit = "git -c user.name=t -c user.email=t@t commit -q --allow-empty -m probe";
    let api = fake_api(vec![ToolCall::bash(commit)]).await;
    let backend = claude_backend(&claude, &api, &folders.root, &folders.home, &folders.data);

    let events = drive(&backend, request, |_| Decision::Allow {
        input: None,
        always: false,
    })
    .await;
    let asked = requests(&events);
    assert_eq!(asked.len(), 1, "{events:#?}");
    assert_eq!(asked[0].tool_name, "Bash");
    assert_eq!(asked[0].input["command"], commit);
    assert_eq!(results(&events)[0].1, ToolStatus::Ok, "{events:#?}");
    assert_eq!(git(&worktree, &["log", "-1", "--format=%s"]), "probe\n");
    assert_eq!(outcome(&events), &done(), "{events:#?}");
}

/// A worker in Plan whose client answers hands its plan to plxd with `ExitPlanMode` (RYA-243),
/// and plxd's allow takes it out of plan mode. In plan mode, Claude Code 2.1.283 sends each
/// command to its auto-mode classifier, which the fake API can't answer, so the worker's first
/// Bash is denied without asking. Once the plan is allowed, the CLI runs in Manual, where the
/// worker's settings allow sandboxed Bash (0013), so the same command runs without asking.
#[tokio::test]
async fn a_plan_worker_hands_its_plan_to_plxd_and_leaves_plan_mode_on_its_allow() {
    const PLAN: &str = "1. Add a README.\n2. Link it from the docs.\n";
    let Some(claude) = std::env::var_os("PLX_SANDBOX_CLAUDE") else {
        eprintln!("skipped: set PLX_SANDBOX_CLAUDE to test the real Claude Code CLI");
        return;
    };
    let folders = Folders::new();
    let (worktree, request, _temp) = worker(&folders, AgentPermission::Plan);
    let probe = ToolCall::bash("sh probe.sh");
    let exit_plan = ToolCall {
        name: "ExitPlanMode",
        input: json!({"plan": PLAN}),
    };
    let api = fake_api(vec![probe.clone(), exit_plan, probe]).await;
    let backend = claude_backend(&claude, &api, &folders.root, &folders.home, &folders.data);

    let events = drive(&backend, request, |asked| {
        if asked.tool_name == "ExitPlanMode" {
            Decision::Allow {
                input: None,
                always: false,
            }
        } else {
            Decision::Deny {
                message: "Only the plan is approved.".into(),
                interrupt: false,
            }
        }
    })
    .await;
    let asked = requests(&events);
    assert_eq!(asked.len(), 1, "only the plan asks: {events:#?}");
    assert_eq!(asked[0].tool_name, "ExitPlanMode");
    assert!(asked[0].interactive, "{events:#?}");
    assert_eq!(asked[0].input["plan"], PLAN);
    assert_eq!(asked[0].call_id.as_deref(), Some("toolu_01ParallaxProbe1"));
    let results = results(&events);
    assert_eq!(results.len(), 3, "{events:#?}");
    assert_ne!(
        results[0].1,
        ToolStatus::Ok,
        "plan mode ran it: {events:#?}"
    );
    assert_eq!(results[1].1, ToolStatus::Ok, "{events:#?}");
    assert_eq!(results[2].1, ToolStatus::Ok, "{events:#?}");
    assert!(results[2].2.contains("probe-ran"), "{events:#?}");
    assert_eq!(
        fs::read_to_string(worktree.join("ran")).unwrap(),
        "x\n",
        "only the command after the allow ran"
    );
    assert_eq!(outcome(&events), &done(), "{events:#?}");
}

/// A worker keeps its plan with Claude Code's task tools (RYA-248). Its `--tools` names them, so
/// 2.1.283 offers them even on a model it would otherwise give no todo tool, and its init passes
/// plxd's check. The CLI writes the list itself, in its configuration folder outside the
/// worktree, which the worker's commands still can't read. The list is the session's own, though
/// the global config, which even `--restricted` reads, names a shared one (RYA-251).
#[tokio::test]
async fn a_worker_keeps_its_plan_with_the_task_tools_where_its_commands_cannot_read_it() {
    let Some(claude) = std::env::var_os("PLX_SANDBOX_CLAUDE") else {
        eprintln!("skipped: set PLX_SANDBOX_CLAUDE to test the real Claude Code CLI");
        return;
    };
    let folders = Folders::new();
    let (worktree, mut request, _temp) = worker(&folders, AgentPermission::Edit);
    request.model = Some("claude-opus-5-5".into());
    share_the_task_list(&folders.home, &[]);
    // In a script, so only the sandbox, not Claude Code's own checks, can stop the read. It names
    // its `HOME`, so a read that fails only because `HOME` is wrong can't pass.
    fs::write(
        worktree.join("peek.sh"),
        "echo \"home=$HOME\"\ncat \"$HOME\"/.claude/tasks/*/1.json\necho peeked\n",
    )
    .unwrap();
    let task = |name, input| ToolCall { name, input };
    let api = fake_api(vec![
        task(
            "TaskCreate",
            json!({
                "subject": "Add tests",
                "description": "Cover the parser",
                "activeForm": "Adding tests",
            }),
        ),
        task(
            "TaskUpdate",
            json!({"taskId": "1", "status": "in_progress"}),
        ),
        ToolCall::bash("sh peek.sh"),
        task("TaskUpdate", json!({"taskId": "1", "status": "completed"})),
    ])
    .await;
    let backend = claude_backend(&claude, &api, &folders.root, &folders.home, &folders.data);

    let events = drive(&backend, request, |asked| {
        panic!("an Accept Edits worker asked: {asked:?}")
    })
    .await;
    let results = results(&events);
    assert_eq!(results.len(), 4, "{events:#?}");
    assert_eq!(
        results[0],
        (
            "toolu_01ParallaxProbe",
            ToolStatus::Ok,
            "Task #1 created successfully: Add tests"
        ),
        "{events:#?}"
    );
    assert_eq!(results[1].2, "Updated task #1 status", "{events:#?}");
    let home = format!("home={}", folders.home.display());
    assert!(results[2].2.lines().any(|line| line == home), "{events:#?}");
    assert!(results[2].2.contains("peeked"), "{events:#?}");
    assert!(!results[2].2.contains("Cover the parser"), "{events:#?}");
    assert_eq!(results[3].2, "Updated task #1 status", "{events:#?}");
    assert_eq!(outcome(&events), &done(), "{events:#?}");

    let session = events
        .iter()
        .find_map(|event| match event {
            Event::SessionStarted { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        })
        .unwrap();
    let tasks = folders.home.join(".claude/tasks");
    let list = tasks.join(session);
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(list.join("1.json")).unwrap()).unwrap();
    assert_eq!(saved["subject"], "Add tests");
    assert_eq!(saved["status"], "completed");
    assert!(!tasks.join(SHARED_LIST).exists());
    // Nothing of it in the worktree. On Linux, the sandbox leaves an empty `.claude` there: the
    // mount point that keeps commands from creating Claude Code's settings files, which git
    // doesn't track.
    assert_eq!(files_in(&worktree.join(".claude")), Vec::<PathBuf>::new());
}

/// A coordinator in Manual, with the prompt channel, keeps its plan with the task tools on a model
/// outside 2.1.283's built-in list (RYA-249). It has no `--tools`, so its `--allowedTools` is what
/// turns them on.
#[tokio::test]
async fn a_coordinator_keeps_its_plan_with_the_task_tools_on_any_model() {
    let Some(claude) = std::env::var_os("PLX_SANDBOX_CLAUDE") else {
        eprintln!("skipped: set PLX_SANDBOX_CLAUDE to test the real Claude Code CLI");
        return;
    };
    let folders = Folders::new();
    let request = RunRequest {
        model: Some("claude-opus-5-5".into()),
        ..coordinator(&folders.project, &folders.data)
    };
    plans_with_the_task_tools(&claude, &folders, request).await;
}

/// A worker in Bypass Permissions, which has no `--tools` either, does the same (RYA-249).
#[tokio::test]
async fn a_bypass_worker_keeps_its_plan_with_the_task_tools_on_any_model() {
    let Some(claude) = std::env::var_os("PLX_SANDBOX_CLAUDE") else {
        eprintln!("skipped: set PLX_SANDBOX_CLAUDE to test the real Claude Code CLI");
        return;
    };
    let folders = Folders::new();
    let (_worktree, mut request, _temp) = worker(&folders, AgentPermission::Bypass);
    request.model = Some("claude-opus-5-5".into());
    plans_with_the_task_tools(&claude, &folders, request).await;
}

/// Runs `request` against a fake API that asks for `TaskCreate`, `TaskUpdate`, and `TaskList`.
/// Each answers as 2.1.283's task tools do, none asks plxd, and the CLI keeps the session's list
/// in its configuration folder, `.claude` in `HOME`, though the global config and the user's, the
/// project's, and the local settings all name a shared one (RYA-251).
async fn plans_with_the_task_tools(claude: &OsStr, folders: &Folders, request: RunRequest) {
    share_the_task_list(&folders.home, &[&request.cwd]);
    let task = |name, input| ToolCall { name, input };
    let api = fake_api(vec![
        task(
            "TaskCreate",
            json!({
                "subject": "Add tests",
                "description": "Cover the parser",
                "activeForm": "Adding tests",
            }),
        ),
        task(
            "TaskUpdate",
            json!({"taskId": "1", "status": "in_progress"}),
        ),
        task("TaskList", json!({})),
    ])
    .await;
    let backend = claude_backend(claude, &api, &folders.root, &folders.home, &folders.data);

    let events = drive(&backend, request, |asked| {
        panic!("a task tool asked: {asked:?}")
    })
    .await;
    assert_eq!(
        results(&events),
        [
            (
                "toolu_01ParallaxProbe",
                ToolStatus::Ok,
                "Task #1 created successfully: Add tests"
            ),
            (
                "toolu_01ParallaxProbe1",
                ToolStatus::Ok,
                "Updated task #1 status"
            ),
            (
                "toolu_01ParallaxProbe2",
                ToolStatus::Ok,
                "#1 [in_progress] Add tests"
            ),
        ],
        "{events:#?}"
    );
    assert_eq!(outcome(&events), &done(), "{events:#?}");

    let session = events
        .iter()
        .find_map(|event| match event {
            Event::SessionStarted { session_id, .. } => Some(session_id.as_str()),
            _ => None,
        })
        .unwrap();
    let tasks = folders.home.join(".claude/tasks");
    let list = tasks.join(session);
    let saved: Value =
        serde_json::from_str(&fs::read_to_string(list.join("1.json")).unwrap()).unwrap();
    assert_eq!(saved["subject"], "Add tests");
    assert_eq!(saved["status"], "in_progress");
    assert!(!tasks.join(SHARED_LIST).exists());
}

/// Names [`SHARED_LIST`] in `CLAUDE_CODE_TASK_LIST_ID` in the `env` of the global config and the
/// user's settings in `home`, and of the project's and the local settings in each of `projects`.
/// Claude Code 2.1.283 copies each of those it reads into its own process, and then a run would
/// share one list with every session that does (RYA-251).
fn share_the_task_list(home: &Path, projects: &[&Path]) {
    let settings = json!({"env": {"CLAUDE_CODE_TASK_LIST_ID": SHARED_LIST}}).to_string();
    let mut files = vec![
        home.join(".claude.json"),
        home.join(".claude/settings.json"),
    ];
    for project in projects {
        files.push(project.join(".claude/settings.json"));
        files.push(project.join(".claude/settings.local.json"));
    }
    for file in files {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, &settings).unwrap();
    }
}

/// Every file under `folder`, which may not exist.
fn files_in(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(folder) else {
        return Vec::new();
    };
    entries
        .map(|entry| entry.unwrap().path())
        .flat_map(|path| {
            if path.is_dir() {
                files_in(&path)
            } else {
                vec![path]
            }
        })
        .collect()
}
