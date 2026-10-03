//! The Claude backend against a fake `claude` on `PATH` that replays synthetic fixtures, so every
//! test spawns a real process through the supervisor. No test runs the real CLI.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parallax_protocol::{CoordinatorThreadId, ProjectId};
use serde_json::Value;
use tempfile::TempDir;

use super::stream::{Ask, Step, Translator};
use super::{
    ClaudeBackend, EXIT_PLAN_MODE, NO_WRITE_ARGS, PLAN_WORKER_TOOL_LIST, PLAN_WORKSPACE_WRITE_ARGS,
    PROMPT_TOOL_ARGS, TODO_TOOLS, WORKER_TOOL_LIST, WORKER_TOOLS, WORKSPACE_WRITE_ARGS,
    no_write_settings, write_env_file,
};
use crate::backend::event::{MAX_ALWAYS_ALLOW_RULE_BYTES, MAX_ALWAYS_ALLOW_RULES};
use crate::backend::process::{CancelPolicy, Environment, Launcher, SpawnError};
use crate::backend::{
    AccountRef, AgentEffort, AgentPermission, Answer, ApiKey, ApprovalRequest, Backend,
    CoordinatorTools, Credential, Decision, Event, EventStream, FailureKind, FollowUp,
    ImageMediaType, LimitStatus, LimitWindow, ModelUsage, Outcome, PromptImage, Resume, RunId,
    RunRequest, SendError, StartError, Started, TodoItem, TodoStatus, ToolPolicy, ToolStatus,
    TurnId, Usage, WarningKind, WorkerSandbox,
};
use crate::mcp;
use crate::paths::DataDir;

const SESSION: &str = "5b1e3c9a-8f2d-4c6e-9a1b-3d7f0e2c4a68";
const TURN_1: &str = "01997e2a-4c3b-7d10-8a2e-5f6b7c8d9e01";
const TURN_2: &str = "01997e2a-4c3b-7d10-8a2e-5f6b7c8d9e02";
const OPUS: &str = "claude-opus-4-7";
const FAKE_CLAUDE: &str = include_str!("fixtures/fake-claude.sh");

fn fixture(name: &str) -> &'static str {
    match name {
        "read-only" => include_str!("fixtures/read-only.jsonl"),
        "tool-call" => include_str!("fixtures/tool-call.jsonl"),
        "error-result" => include_str!("fixtures/error-result.jsonl"),
        "not-signed-in" => include_str!("fixtures/not-signed-in.jsonl"),
        "rate-limited" => include_str!("fixtures/rate-limited.jsonl"),
        "resume" => include_str!("fixtures/resume.jsonl"),
        "api-key-source" => include_str!("fixtures/api-key-source.jsonl"),
        "api-key-completed" => include_str!("fixtures/api-key-completed.jsonl"),
        "write-tools" => include_str!("fixtures/write-tools.jsonl"),
        "malformed" => include_str!("fixtures/malformed.jsonl"),
        "follow-up-folded" => include_str!("fixtures/follow-up-folded.jsonl"),
        "follow-up-turns" => include_str!("fixtures/follow-up-turns.jsonl"),
        "cancel" => include_str!("fixtures/cancel.jsonl"),
        "stubborn" => include_str!("fixtures/stubborn.jsonl"),
        "follow-up-no-echo" => include_str!("fixtures/follow-up-no-echo.jsonl"),
        "provider" => include_str!("fixtures/provider.jsonl"),
        "subprocess-env" => include_str!("fixtures/subprocess-env.jsonl"),
        "scrub-mode" => include_str!("fixtures/scrub-mode.jsonl"),
        "approval" => include_str!("fixtures/approval.jsonl"),
        "approval-withdrawn" => include_str!("fixtures/approval-withdrawn.jsonl"),
        "exit-plan" => include_str!("fixtures/exit-plan.jsonl"),
        "approval-exit" => include_str!("fixtures/approval-exit.jsonl"),
        "worker-exit-plan" => include_str!("fixtures/worker-exit-plan.jsonl"),
        "worker-tasks" => include_str!("fixtures/worker-tasks.jsonl"),
        "coordinator-tasks" => include_str!("fixtures/coordinator-tasks.jsonl"),
        other => panic!("no fixture {other}"),
    }
}

/// Inherited variables that could pick Claude's credentials, provider, endpoint, or account, one
/// of each kind, and the variable that would share a run's task list (RYA-251). None may reach the
/// CLI, whatever the run's account or policy.
const INHERITED_CREDENTIALS: &[(&str, &str)] = &[
    ("ANTHROPIC_API_KEY", "parallax-test-not-a-key"),
    ("ANTHROPIC_AUTH_TOKEN", "parallax-test-not-a-bearer"),
    ("CLAUDE_CODE_OAUTH_TOKEN", "parallax-test-not-a-token"),
    (
        "CLAUDE_CODE_OAUTH_REFRESH_TOKEN",
        "parallax-test-not-a-token",
    ),
    ("CLAUDE_CODE_USE_BEDROCK", "1"),
    ("CLAUDE_CODE_USE_VERTEX", "1"),
    ("CLAUDE_CODE_USE_FOUNDRY", "1"),
    ("ANTHROPIC_BASE_URL", "https://example.invalid"),
    ("ANTHROPIC_UNIX_SOCKET", "/tmp/parallax-test.sock"),
    ("ANTHROPIC_PROFILE", "work"),
    ("ANTHROPIC_FEDERATION_RULE_ID", "parallax-test-rule"),
    ("ANTHROPIC_ORGANIZATION_ID", "parallax-test-org"),
    ("AWS_BEARER_TOKEN_BEDROCK", "parallax-test-not-a-token"),
    ("CLAUDE_CONFIG_DIR", "/tmp/parallax-test-inherited-config"),
    ("CLAUDE_CODE_TASK_LIST_ID", "parallax-test-shared-list"),
];

fn turn(id: &str) -> TurnId {
    id.parse().unwrap()
}

/// A fake `claude` on the launcher's `PATH`, in a folder that also holds what it records.
struct Fake {
    dir: TempDir,
    backend: ClaudeBackend,
}

impl Fake {
    fn new(fixture_name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let program = bin.join("claude");
        fs::write(&program, FAKE_CLAUDE).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let fixture_path = root.join("fixture.jsonl");
        fs::write(&fixture_path, fixture(fixture_name)).unwrap();
        let base: Environment = [
            ("PATH", format!("{}:/usr/bin:/bin", bin.display())),
            ("FAKE_CLAUDE_DIR", root.display().to_string()),
            ("FAKE_CLAUDE_FIXTURE", fixture_path.display().to_string()),
            ("SSH_CONNECTION", "10.0.0.2 50000 10.0.0.1 22".into()),
            ("KEPT", "yes".into()),
            // Set in plxd's own environment: a no-write run sets it anyway, and a worker must
            // not get it (RYA-112).
            ("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB", "0".into()),
        ]
        .into_iter()
        .chain(
            INHERITED_CREDENTIALS
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned())),
        )
        .collect();
        let launcher = Launcher::new(DataDir::new(root.join("data")).unwrap(), base);
        Self {
            dir,
            backend: ClaudeBackend::new(launcher),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn recorded(&self, name: &str) -> String {
        fs::read_to_string(self.root().join(name)).unwrap_or_default()
    }

    fn argv(&self) -> Vec<String> {
        self.recorded("argv").lines().map(str::to_owned).collect()
    }

    fn env(&self) -> Vec<String> {
        self.recorded("env").lines().map(str::to_owned).collect()
    }

    /// Checks that no inherited credential reached the CLI. `CLAUDE_CONFIG_DIR` may only have
    /// the value Parallax set on purpose.
    fn assert_no_inherited_credentials(&self, config_dir: Option<&str>) {
        let env = self.env();
        for (name, value) in INHERITED_CREDENTIALS {
            let prefix = format!("{name}=");
            let leaked: Vec<&String> = env
                .iter()
                .filter(|var| var.starts_with(&prefix))
                .filter(|var| *name != "CLAUDE_CONFIG_DIR" || var.ends_with(value))
                .collect();
            assert!(leaked.is_empty(), "{name} leaked: {leaked:?}");
        }
        let config = env
            .iter()
            .find_map(|var| var.strip_prefix("CLAUDE_CONFIG_DIR="));
        assert_eq!(config, config_dir);
    }

    /// The user messages the CLI read from stdin.
    fn stdin(&self) -> Vec<Value> {
        self.recorded("stdin")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn request(cwd: &Path) -> RunRequest {
    RunRequest {
        run_id: RunId::generate(),
        turn_id: Some(turn(TURN_1)),
        cwd: cwd.to_owned(),
        prompt: "Summarize the README.\nKeep it short.".into(),
        images: Vec::new(),
        policy: ToolPolicy::NoWrite,
        sandbox: None,
        account: AccountRef {
            id: "claude-max".into(),
            credential: Credential::Subscription { config_home: None },
        },
        resume: None,
        model: None,
        effort: None,
        permission: None,
        context_window: None,
        fast: None,
        coordinator_tools: None,
        thread_tools: None,
        approvals: false,
        thread: false,
    }
}

async fn launch(backend: &dyn Backend, request: RunRequest) -> Started {
    let turn_id = request.turn_id;
    let mut started = backend.start(request).unwrap();
    assert_eq!(
        next(&mut started.events).await,
        Event::TurnStarted { turn_id }
    );
    started
}

async fn next(events: &mut EventStream) -> Event {
    tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .expect("no event within 10 s")
        .expect("the stream ended")
}

async fn rest(events: &mut EventStream) -> Vec<Event> {
    let mut all = Vec::new();
    loop {
        let event = next(events).await;
        let terminal = event.is_terminal();
        all.push(event);
        if terminal {
            assert!(events.next().await.is_none(), "Finished must be last");
            return all;
        }
    }
}

async fn run(fake: &Fake, request: RunRequest) -> Vec<Event> {
    let mut events = launch(&fake.backend, request).await.events;
    rest(&mut events).await
}

fn outcome(events: &[Event]) -> &Outcome {
    match events.last() {
        Some(Event::Finished { outcome, .. }) => outcome,
        other => panic!("expected Finished last, got {other:?}"),
    }
}

fn failure(events: &[Event]) -> (FailureKind, &str) {
    match outcome(events) {
        Outcome::Failed(failure) => (failure.failure, failure.message.as_str()),
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn texts(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn warnings(events: &[Event]) -> Vec<WarningKind> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Warning { warning, .. } => Some(*warning),
            _ => None,
        })
        .collect()
}

fn tokens(input: u64, output: u64, read: u64, write: u64, cost: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: read,
        cache_write_tokens: write,
        cost_usd_micros: Some(cost),
    }
}

#[tokio::test]
async fn a_read_only_run_maps_the_stream_and_uses_the_no_write_policy() {
    let fake = Fake::new("read-only");
    let cwd = fake.root();
    let all = run(&fake, request(&cwd)).await;
    assert_eq!(
        all,
        [
            Event::SessionStarted {
                session_id: SESSION.into(),
                model: Some(OPUS.into()),
                api_key_source: Some("none".into()),
            },
            Event::Reasoning {
                message_id: Some("msg_01Rd1".into()),
                text: "The user wants a summary of the README.".into(),
            },
            Event::Text {
                message_id: Some("msg_01Rd1".into()),
                text: "The README describes Parallax, a macOS editor with a host daemon.".into(),
            },
            Event::RateLimit(LimitWindow {
                window: "five_hour".into(),
                duration_minutes: Some(300),
                used_percent: Some(12.0),
                status: LimitStatus::Allowed,
                resets_at: Some("2026-09-25T19:00:00Z".parse().unwrap()),
            }),
            Event::Usage(ModelUsage {
                model: Some(OPUS.into()),
                usage: tokens(12, 180, 14000, 3000, 42_100),
            }),
            Event::TurnFinished {
                turn_id: Some(turn(TURN_1)),
                result: Some(
                    "The README describes Parallax, a macOS editor with a host daemon.".into()
                ),
            },
            Event::Finished {
                outcome: Outcome::Completed {
                    result: Some(
                        "The README describes Parallax, a macOS editor with a host daemon.".into()
                    ),
                },
                usage_totals: vec![ModelUsage {
                    model: Some(OPUS.into()),
                    usage: tokens(12, 180, 14000, 3000, 42_100),
                }],
            },
        ]
    );

    let mut expected: Vec<&str> = vec![
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--input-format",
        "stream-json",
    ];
    let settings = no_write_settings().to_string();
    expected.extend(NO_WRITE_ARGS);
    expected.extend(["--settings", &settings]);
    assert_eq!(fake.argv(), expected);
    assert_eq!(
        NO_WRITE_ARGS.join(" "),
        "--tools Read,Glob,Grep --setting-sources user --strict-mcp-config --permission-mode \
         dontAsk",
        "0004's no-write flags, before its settings"
    );
    let env = fake.env();
    let working_dir = format!("PWD={}", cwd.display());
    assert!(env.contains(&working_dir), "{env:?}");
    fake.assert_no_inherited_credentials(None);
    assert!(!env.iter().any(|var| var.starts_with("SSH_CONNECTION=")));
    assert!(!env.iter().any(|var| var.starts_with("CLAUDE_ENV_FILE=")));
    for set in [
        "CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1",
        "CLAUDE_CODE_STARTUP_FAILURE_RESULTS=1",
        "KEPT=yes",
    ] {
        assert!(env.iter().any(|var| var == set), "{set} missing: {env:?}");
    }

    assert_eq!(
        fake.stdin(),
        [serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": "Summarize the README.\nKeep it short."},
            "parent_tool_use_id": null,
            "uuid": TURN_1,
        })],
        "the prompt goes on stdin, not in argv"
    );
}

/// RYA-176: a plain no-write run has hooks off (0004) and can't read Claude Code's shared temp
/// folder, which holds every session's files, in either spelling.
#[test]
fn a_no_write_run_cannot_read_claudes_shared_temp_folder() {
    let args = super::arguments(&request(Path::new("/repo"))).unwrap();
    let at = args.iter().position(|arg| arg == "--settings").unwrap();
    let settings: Value = serde_json::from_str(args[at + 1].to_str().unwrap()).unwrap();
    let uid = rustix::process::getuid().as_raw();
    assert_eq!(
        settings,
        serde_json::json!({
            "disableAllHooks": true,
            "permissions": {"deny": [
                format!("Read(//tmp/claude-{uid}/**)"),
                format!("Read(//private/tmp/claude-{uid}/**)"),
            ]},
            "env": {"CLAUDE_CODE_TASK_LIST_ID": ""},
        })
    );
}

/// 0027: a coordinator runs as Claude Code in its mode, and without
/// `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB`, which would force "default"; it doesn't inherit it either.
#[tokio::test]
async fn a_coordinator_runs_in_its_mode_without_the_subprocess_scrub() {
    let fake = Fake::new("tool-call");
    let cwd = fake.root();
    let request = RunRequest {
        coordinator_tools: Some(CoordinatorTools {
            program: PathBuf::from("/Applications/Parallax.app/Contents/Resources/plxd"),
            data_dir: cwd.join("data"),
            project: ProjectId::generate(),
            thread: CoordinatorThreadId::generate(),
        }),
        ..request(&cwd)
    };
    let expected: Vec<String> = super::arguments(&request)
        .unwrap()
        .into_iter()
        .map(|arg| arg.into_string().unwrap())
        .collect();
    run(&fake, request).await;
    assert_eq!(fake.argv(), expected);
    fake.assert_no_inherited_credentials(None);
    let env = fake.env();
    assert!(
        !env.iter()
            .any(|var| var.starts_with("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=")),
        "{env:?}"
    );
}

/// A sandbox as #156 builds it, for a worktree at `cwd`.
fn worker_sandbox(cwd: &Path) -> WorkerSandbox {
    WorkerSandbox::for_worktree(
        Path::new("/Users/u"),
        Path::new("/Users/u/Library/Application Support/parallax"),
        cwd,
        Path::new("/Users/u/src/app/.git"),
        Path::new("/Users/u/Library/Application Support/parallax/context/p"),
        Path::new("/tmp/parallax-625c7f6d/Ab12Cd"),
    )
}

/// A worker's policy, sandbox, model, effort, and second account reached the CLI.
fn assert_worker_invocation(fake: &Fake) {
    let argv = fake.argv();
    let cwd = fake.root().display().to_string();
    let mut expected: Vec<&str> = WORKSPACE_WRITE_ARGS.to_vec();
    expected.extend(["--permission-mode", "acceptEdits", "--settings"]);
    assert_eq!(argv[6..6 + expected.len()], expected);
    assert_eq!(
        WORKSPACE_WRITE_ARGS.join(" "),
        "--restricted --tools Read,Edit,Write,Glob,Grep,NotebookEdit,Bash,WebFetch,WebSearch,\
         TodoWrite,TaskCreate,TaskGet,TaskList,TaskUpdate --strict-mcp-config",
        "0013's worker policy, exactly"
    );
    assert_eq!(WORKER_TOOL_LIST, WORKER_TOOLS.join(","));

    let settings: Value = serde_json::from_str(&argv[6 + expected.len()]).unwrap();
    let mut deny_read: Vec<String> = worker_sandbox(Path::new(&cwd))
        .unreadable
        .iter()
        .map(|path| path.display().to_string())
        .collect();
    deny_read.push("/tmp/claude-second-account".into());
    let uid = rustix::process::getuid().as_raw();
    assert_eq!(
        settings,
        serde_json::json!({
            "disableAllHooks": true,
            "permissions": {
                "allow": ["Bash", "WebFetch(domain:*)", "WebSearch"],
                "deny": [
                    "WebFetch(domain:localhost)",
                    "WebFetch(domain:127.0.0.1)",
                    "WebFetch(domain:[::1])",
                    "WebFetch(domain:0.0.0.0)",
                    "WebFetch(domain:[::])",
                ],
            },
            "sandbox": {
                "enabled": true,
                "failIfUnavailable": true,
                "autoAllowBashIfSandboxed": true,
                "allowUnsandboxedCommands": false,
                "excludedCommands": [],
                "network": {
                    "strictAllowlist": true,
                    "deniedDomains": ["localhost", "127.0.0.1", "[::1]", "0.0.0.0", "[::]"],
                    "allowLocalBinding": false,
                },
                "filesystem": {
                    "denyRead": deny_read,
                    "allowRead": [
                        cwd,
                        "/Users/u/Library/Application Support/parallax/context/p",
                        format!("{cwd}/.git"),
                        "/Users/u/src/app/.git",
                        format!("/tmp/parallax-625c7f6d/Ab12Cd/claude-{uid}"),
                    ],
                    "denyWrite": [
                        format!("{cwd}/.git"),
                        "/Users/u/src/app/.git",
                        "/tmp/claude",
                        "/private/tmp/claude",
                        "~/.npm/_logs",
                        "~/.claude/debug",
                    ],
                },
                "credentials": {"envVars": [
                    {"name": "ANTHROPIC_API_KEY", "mode": "deny"},
                    {"name": "CLAUDE_CODE_MESSAGING_TOKEN", "mode": "deny"},
                ]},
            },
            "env": {"CLAUDE_CODE_TASK_LIST_ID": ""},
        })
    );
    assert_eq!(
        argv[7 + expected.len()..],
        [
            "--add-dir",
            "/Users/u/Library/Application Support/parallax/context/p",
            "--model",
            "claude-sonnet-4-6",
            "--effort",
            "high",
        ]
    );
    for flag in [
        "--setting-sources",
        "--allowedTools",
        "--dangerously-skip-permissions",
    ] {
        assert!(!argv.iter().any(|arg| arg == flag), "{flag}: {argv:?}");
    }
    fake.assert_no_inherited_credentials(Some("/tmp/claude-second-account"));
    // On Linux the flag would widen the sandbox's writes (RYA-20); `credentials` stands in for it.
    // The fake's base environment sets it, so this also checks that a worker drops it (RYA-112).
    let env = fake.env();
    assert!(
        !env.iter()
            .any(|var| var.starts_with("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=")),
        "{env:?}"
    );
    // The run's own temp folder (RYA-130).
    let temp = "CLAUDE_CODE_TMPDIR=/tmp/parallax-625c7f6d/Ab12Cd".to_owned();
    assert!(env.contains(&temp), "{env:?}");
}

/// Claude Code gives commands its temp folder only while `<folder>/claude-<uid>` fits in 44
/// bytes, and the shared one otherwise, so a longer folder refuses the worker (RYA-130).
#[test]
fn a_worker_s_temp_folder_must_leave_claude_code_room() {
    let uid = rustix::process::getuid().as_raw().to_string();
    let fits = format!(
        "/tmp/{}",
        "x".repeat(44 - "/tmp//claude-".len() - uid.len())
    );
    assert_eq!(
        super::worker_temp(Path::new(&fits)).unwrap(),
        Path::new(&fits)
    );
    let error = super::worker_temp(Path::new(&format!("{fits}x"))).unwrap_err();
    assert!(
        error.to_string().contains("every session shares"),
        "{error}"
    );
    // macOS's canonical `/private/tmp` is spelled `/tmp`, where it links, to save the room.
    let canonical = super::worker_temp(Path::new(&format!("/private{fits}")));
    if cfg!(target_os = "macos") {
        assert_eq!(canonical.unwrap(), Path::new(&fits));
    } else {
        assert!(canonical.is_err());
    }
}

/// A worker gets a script that keeps the CLI's `PATH` in its Bash commands (RYA-126), in the data
/// folder, and it's gone once the run has ended.
#[tokio::test]
async fn a_worker_s_env_file_restores_its_path_and_ends_with_the_run() {
    let fake = Fake::new("tool-call");
    let mut request = request(&fake.root());
    request.policy = ToolPolicy::WorkspaceWrite;
    request.sandbox = Some(worker_sandbox(&fake.root()));
    run(&fake, request).await;
    let env_file = fake
        .env()
        .iter()
        .find_map(|var| var.strip_prefix("CLAUDE_ENV_FILE="))
        .map(PathBuf::from)
        .unwrap();
    assert_eq!(
        env_file.parent(),
        Some(fake.root().join("data/tmp").as_path())
    );
    assert_eq!(
        fake.recorded("env-file"),
        format!(
            "export PATH='{}/bin:/usr/bin:/bin'${{PATH:+:$PATH}}\n",
            fake.root().display()
        )
    );
    assert!(!env_file.exists());
}

/// The script quotes the `PATH` it restores, keeps what the shell had after it, and is private.
#[test]
fn the_env_file_puts_the_path_back_in_front_and_is_private() {
    let dir = tempfile::tempdir().unwrap();
    let file = write_env_file(&dir.path().join("tmp"), "/it's/bin:/usr/bin".as_ref()).unwrap();
    let mode = fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", ". \"$0\"; printf %s \"$PATH\""])
        .arg(&*file)
        .env("PATH", "/set/by/zshenv")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "/it's/bin:/usr/bin:/set/by/zshenv"
    );
    let path = file.to_path_buf();
    drop(file);
    assert!(!path.exists());
}

#[test]
fn a_worker_s_settings_deny_every_name_for_this_mac_to_commands_and_web_fetch() {
    let sandbox = worker_sandbox(Path::new("/Users/u/wt"));
    let settings = super::worker_settings(&sandbox, Path::new("/Users/u/wt"), None);
    let list = |pointer: &str| -> Vec<String> {
        settings
            .pointer(pointer)
            .and_then(Value::as_array)
            .unwrap()
            .iter()
            .map(|entry| entry.as_str().unwrap().to_owned())
            .collect()
    };
    let denied_hosts = list("/sandbox/network/deniedDomains");
    let denied_fetches = list("/permissions/deny");
    for host in ["localhost", "127.0.0.1", "[::1]", "0.0.0.0", "[::]"] {
        assert!(
            denied_hosts.iter().any(|h| h == host),
            "commands reach {host}"
        );
        let rule = format!("WebFetch(domain:{host})");
        assert!(denied_fetches.contains(&rule), "WebFetch reaches {host}");
    }
    assert_eq!(
        list("/permissions/allow"),
        ["Bash", "WebFetch(domain:*)", "WebSearch"]
    );
}

/// RYA-97, 0027: a worker's permission picks Claude Code's mode of the same name, and its sandbox
/// settings stay the same, except in bypass, where Claude Code refuses `--restricted` and the
/// worker runs as full Claude Code. A no-write run's mode is fixed (0004), so it takes no
/// permission.
#[test]
fn a_worker_s_permission_picks_its_mode_inside_the_same_sandbox_but_bypass() {
    let cwd = Path::new("/Users/u/wt");
    let mut worker = request(cwd);
    worker.policy = ToolPolicy::WorkspaceWrite;
    worker.sandbox = Some(worker_sandbox(cwd));
    let args = |permission| -> Vec<String> {
        let request = RunRequest {
            permission,
            ..worker.clone()
        };
        super::arguments(&request)
            .unwrap()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    };
    let after = |args: &[String], flag: &str| {
        let at = args.iter().position(|arg| arg == flag).unwrap();
        args[at + 1].clone()
    };
    let default = args(None);
    let edit = args(Some(AgentPermission::Edit));
    assert_eq!(default, edit);
    assert_eq!(after(&edit, "--permission-mode"), "acceptEdits");
    for (permission, mode) in [
        (AgentPermission::Auto, "auto"),
        (AgentPermission::Manual, "default"),
        (AgentPermission::Plan, "plan"),
    ] {
        let args = args(Some(permission));
        assert_eq!(after(&args, "--permission-mode"), mode);
        assert_eq!(after(&args, "--settings"), after(&edit, "--settings"));
        assert!(args.starts_with(&edit[..super::BASE_ARGS.len() + WORKSPACE_WRITE_ARGS.len()]));
        assert_eq!(
            args.iter()
                .filter(|arg| *arg == "--permission-mode")
                .count(),
            1
        );
    }
    let bypass = args(Some(AgentPermission::Bypass));
    assert_eq!(after(&bypass, "--permission-mode"), "bypassPermissions");
    for flag in ["--restricted", "--tools", "--strict-mcp-config"] {
        assert!(!bypass.contains(&flag.to_owned()), "{flag}: {bypass:?}");
    }
    // None of the sandbox's settings, only its task list's (RYA-251).
    assert_eq!(
        after(&bypass, "--settings"),
        r#"{"env":{"CLAUDE_CODE_TASK_LIST_ID":""}}"#
    );
    assert_eq!(
        bypass.iter().filter(|arg| *arg == "--add-dir").count(),
        edit.iter().filter(|arg| *arg == "--add-dir").count()
    );

    let no_write = RunRequest {
        permission: Some(AgentPermission::Edit),
        ..request(cwd)
    };
    assert!(matches!(
        super::arguments(&no_write),
        Err(StartError::Invalid(_))
    ));
}

/// RYA-251: every run, whatever its policy, mode, or prompt channel, gets exactly one
/// `--settings`, since Claude Code keeps only the last, and it sets `CLAUDE_CODE_TASK_LIST_ID`
/// empty. That beats a value in the `env` of the global config or of the user's, the project's,
/// or the local settings, so the run keeps its session's own task list.
#[test]
fn every_run_s_one_settings_keep_its_task_list_its_own() {
    let cwd = Path::new("/Users/u/wt");
    let mut worker = request(cwd);
    worker.policy = ToolPolicy::WorkspaceWrite;
    worker.sandbox = Some(worker_sandbox(cwd));
    let coordinator_run = coordinator(cwd);
    let mut runs = vec![request(cwd)];
    for permission in [
        None,
        Some(AgentPermission::Edit),
        Some(AgentPermission::Auto),
        Some(AgentPermission::Manual),
        Some(AgentPermission::Plan),
        Some(AgentPermission::Bypass),
    ] {
        for approvals in [false, true] {
            for base in [&worker, &coordinator_run] {
                runs.push(RunRequest {
                    permission,
                    approvals,
                    ..base.clone()
                });
            }
        }
    }
    for run in runs {
        let args: Vec<String> = super::arguments(&run)
            .unwrap()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        let settings: Vec<Value> = args
            .iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "--settings")
            .map(|(at, _)| serde_json::from_str(&args[at + 1]).unwrap())
            .collect();
        assert_eq!(settings.len(), 1, "{args:?}");
        assert_eq!(
            settings[0]["env"],
            serde_json::json!({"CLAUDE_CODE_TASK_LIST_ID": ""}),
            "{args:?}"
        );
    }
}

/// Fast mode goes in every run's one `--settings`, the only place headless Claude Code takes it.
#[test]
fn fast_mode_is_set_in_every_run_s_settings() {
    let cwd = Path::new("/Users/u/wt");
    let mut worker = request(cwd);
    worker.policy = ToolPolicy::WorkspaceWrite;
    worker.sandbox = Some(worker_sandbox(cwd));
    let bypass = RunRequest {
        permission: Some(AgentPermission::Bypass),
        ..worker.clone()
    };
    for base in [request(cwd), worker, bypass, coordinator(cwd)] {
        for fast in [None, Some(true), Some(false)] {
            let args: Vec<String> = super::arguments(&RunRequest {
                fast,
                ..base.clone()
            })
            .unwrap()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
            let at = args.iter().position(|arg| arg == "--settings").unwrap();
            let settings: Value = serde_json::from_str(&args[at + 1]).unwrap();
            assert_eq!(settings.get("fastMode").and_then(Value::as_bool), fast);
        }
    }
}

/// A 200k context window caps a native 1M model with `CLAUDE_CODE_DISABLE_1M_CONTEXT`; 1M is
/// those models' default, and any other size is refused.
#[tokio::test]
async fn a_200k_context_window_disables_1m_context() {
    let fake = Fake::new("read-only");
    let cwd = fake.root();
    let other = RunRequest {
        context_window: Some(500_000),
        ..request(&cwd)
    };
    assert!(matches!(
        fake.backend.start(other),
        Err(StartError::Unsupported(_))
    ));
    let capped = RunRequest {
        context_window: Some(200_000),
        ..request(&cwd)
    };
    run(&fake, capped).await;
    assert!(
        fake.env()
            .contains(&"CLAUDE_CODE_DISABLE_1M_CONTEXT=1".to_owned())
    );
}

#[test]
fn a_worker_without_a_usable_sandbox_is_refused_before_anything_runs() {
    let fake = Fake::new("tool-call");
    let mut worker = request(&fake.root());
    worker.policy = ToolPolicy::WorkspaceWrite;
    let refused = |request: RunRequest| match fake.backend.start(request) {
        Err(StartError::Invalid(message)) => message,
        Err(other) => panic!("expected Invalid, got {other:?}"),
        Ok(_) => panic!("expected Invalid, got a run"),
    };
    assert!(refused(worker.clone()).contains("0013"));

    let mut relative = worker_sandbox(&fake.root());
    relative.writable.push("context".into());
    worker.sandbox = Some(relative);
    assert!(refused(worker.clone()).contains("context"));

    let mut empty = worker_sandbox(&fake.root());
    empty.unreadable.clear();
    worker.sandbox = Some(empty);
    assert!(refused(worker.clone()).contains("nothing unreadable"));

    for glob in [
        "/Users/u/src/app[old]/.git",
        "/Users/u/src/app]/.git",
        "/Users/u/src/a*/.git",
        "/Users/u/src/a?/.git",
    ] {
        let mut sandbox = worker_sandbox(&fake.root());
        sandbox.read_only[1] = glob.into();
        worker.sandbox = Some(sandbox);
        assert!(refused(worker.clone()).contains(glob), "{glob}");
    }
    let mut glob_cwd = worker.clone();
    glob_cwd.sandbox = Some(worker_sandbox(&fake.root()));
    glob_cwd.cwd = "/Users/u/src/app[old]".into();
    assert!(refused(glob_cwd).contains("app[old]"));

    worker.sandbox = Some(worker_sandbox(&fake.root()));
    worker.account.credential = Credential::Subscription {
        config_home: Some("second-account".into()),
    };
    assert!(refused(worker.clone()).contains("second-account"));
    worker.account.credential = Credential::Subscription {
        config_home: Some("/Users/u/.claude-*".into()),
    };
    assert!(refused(worker).contains(".claude-*"));
    assert_eq!(fake.argv(), Vec::<String>::new(), "nothing ran");
}

#[tokio::test]
async fn a_worker_run_edits_in_its_cwd_and_reports_its_tool_calls() {
    let fake = Fake::new("tool-call");
    let mut request = request(&fake.root());
    request.policy = ToolPolicy::WorkspaceWrite;
    request.sandbox = Some(worker_sandbox(&fake.root()));
    request.model = Some("claude-sonnet-4-6".into());
    request.effort = Some(AgentEffort::High);
    request.account.credential = Credential::Subscription {
        config_home: Some("/tmp/claude-second-account".into()),
    };
    let all = run(&fake, request).await;
    assert_worker_invocation(&fake);

    let calls: Vec<(&str, &str)> = all
        .iter()
        .filter_map(|event| match event {
            Event::ToolCall { call_id, name, .. } => Some((call_id.as_str(), name.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        [
            ("toolu_01Todo", "TodoWrite"),
            ("toolu_01Read", "Read"),
            ("toolu_01Edit", "Edit"),
            ("toolu_01Bash", "Bash"),
            ("toolu_01Glob", "Glob"),
        ]
    );
    let results: Vec<(&str, ToolStatus, Option<&str>)> = all
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult {
                call_id,
                status,
                output,
            } => Some((call_id.as_str(), *status, output.as_deref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        results[1],
        (
            "toolu_01Read",
            ToolStatus::Ok,
            Some("     1\tfn parse() {}")
        )
    );
    assert_eq!(results[2].1, ToolStatus::Ok);
    assert_eq!(
        results[3].1,
        ToolStatus::Denied,
        "permission_denied names it"
    );
    assert_eq!(
        results[4],
        (
            "toolu_01Glob",
            ToolStatus::Error,
            Some("Error: path does not exist")
        )
    );
    let edit = all.iter().find_map(|event| match event {
        Event::ToolCall { name, input, .. } if name == "Edit" => Some(input),
        _ => None,
    });
    assert_eq!(edit.unwrap()["new_string"], "i < n");
    assert!(all.contains(&Event::TodoList {
        items: vec![
            TodoItem {
                text: "Read the parser".into(),
                status: TodoStatus::Completed
            },
            TodoItem {
                text: "Fix the off-by-one".into(),
                status: TodoStatus::InProgress
            },
            TodoItem {
                text: "Run the tests".into(),
                status: TodoStatus::Pending
            },
        ]
    }));
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("Fixed the off-by-one in the parser. I couldn't run the tests.".into())
        }
    );
}

/// A worker's own init shows scrub mode, which the Linux host check runs in another process and
/// can miss (RYA-118): plxd stops it at init, before its Bash call.
#[tokio::test]
async fn a_worker_whose_init_shows_another_permission_mode_is_stopped_before_any_tool() {
    let fake = Fake::new("scrub-mode");
    let mut request = request(&fake.root());
    request.policy = ToolPolicy::WorkspaceWrite;
    request.sandbox = Some(worker_sandbox(&fake.root()));
    let all = run(&fake, request).await;
    let (kind, message) = failure(&all);
    assert_eq!(kind, FailureKind::PolicyViolation);
    assert!(
        message.contains(r#"permission mode "default" in a worker run instead of "acceptEdits""#),
        "{message}"
    );
    assert!(
        !all.iter()
            .any(|event| matches!(event, Event::ToolCall { .. })),
        "{all:?}"
    );
}

#[tokio::test]
async fn an_error_result_fails_the_run_with_the_cli_s_errors() {
    let fake = Fake::new("error-result");
    let all = run(&fake, request(&fake.root())).await;
    assert!(all.contains(&Event::Notice {
        detail:
            "Claude Code is retrying a request after an error (overloaded), attempt 1 of 10".into()
    }));
    assert!(all.contains(&Event::TurnFinished {
        turn_id: Some(turn(TURN_1)),
        result: None
    }));
    let Outcome::Failed(failure) = outcome(&all) else {
        panic!("{all:?}");
    };
    assert_eq!(failure.failure, FailureKind::VendorError);
    assert_eq!(failure.message, "Reached maximum number of turns (3)");
    assert_eq!(failure.exit.unwrap().code, Some(1));
    assert_eq!(
        failure.stderr_tail.as_deref(),
        Some("Error: Reached max turns (3)")
    );
    assert!(
        all.iter().any(|event| matches!(event, Event::Usage(_))),
        "a failed turn still used tokens"
    );
}

#[tokio::test]
async fn a_signed_out_cli_fails_the_run_as_not_signed_in() {
    let fake = Fake::new("not-signed-in");
    let all = run(&fake, request(&fake.root())).await;
    assert_eq!(
        failure(&all),
        (
            FailureKind::NotSignedIn,
            "Not logged in \u{b7} Please run /login"
        )
    );
}

#[tokio::test]
async fn rate_limits_are_reported_and_a_rejected_one_fails_the_run() {
    let fake = Fake::new("rate-limited");
    let all = run(&fake, request(&fake.root())).await;
    let windows: Vec<&LimitWindow> = all
        .iter()
        .filter_map(|event| match event {
            Event::RateLimit(window) => Some(window),
            _ => None,
        })
        .collect();
    assert_eq!(
        windows,
        [
            &LimitWindow {
                window: "seven_day".into(),
                duration_minutes: Some(10_080),
                used_percent: Some(91.0),
                status: LimitStatus::Warning,
                resets_at: Some("2026-09-30T00:00:00Z".parse().unwrap()),
            },
            &LimitWindow {
                window: "five_hour".into(),
                duration_minutes: Some(300),
                used_percent: Some(100.0),
                status: LimitStatus::Rejected,
                resets_at: Some("2026-09-25T19:00:00Z".parse().unwrap()),
            },
            &LimitWindow {
                window: "seven_day_opus".into(),
                duration_minutes: Some(10_080),
                used_percent: Some(40.0),
                status: LimitStatus::Allowed,
                resets_at: Some("2026-09-30T00:00:00.5Z".parse().unwrap()),
            },
        ]
    );
    assert_eq!(
        failure(&all).0,
        FailureKind::RateLimited,
        "a later allowed window doesn't clear the rejected one"
    );
}

#[tokio::test]
async fn a_model_served_by_another_provider_fails_the_run() {
    let fake = Fake::new("provider");
    let all = run(&fake, request(&fake.root())).await;
    let (kind, message) = failure(&all);
    assert_eq!(kind, FailureKind::UnexpectedApiKey);
    assert!(message.contains("bedrock"), "{message}");
    assert!(
        !all.iter()
            .any(|event| matches!(event, Event::Usage(_) | Event::TurnFinished { .. })),
        "usage billed elsewhere isn't the account's: {all:?}"
    );
}

#[tokio::test]
async fn a_result_without_ids_or_a_queue_count_ends_every_turn() {
    let fake = Fake::new("follow-up-no-echo");
    let Started { run, mut events } = launch(&fake.backend, request(&fake.root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    assert!(matches!(next(&mut events).await, Event::Text { .. }));
    run.send(FollowUp {
        turn_id: turn(TURN_2),
        text: "Fix the tests too.".into(),
        images: Vec::new(),
    })
    .unwrap();
    let all = rest(&mut events).await;
    let finished: Vec<Option<TurnId>> = all
        .iter()
        .filter_map(|event| match event {
            Event::TurnFinished { turn_id, .. } => Some(*turn_id),
            _ => None,
        })
        .collect();
    assert_eq!(finished, [Some(turn(TURN_1)), Some(turn(TURN_2))]);
    assert!(matches!(outcome(&all), Outcome::Completed { .. }));
}

#[tokio::test]
async fn a_resumed_session_reports_only_what_it_adds() {
    let fake = Fake::new("resume");
    let mut request = request(&fake.root());
    let baseline = vec![ModelUsage {
        model: Some(OPUS.into()),
        usage: tokens(1000, 200, 25_000, 4000, 100_000),
    }];
    request.resume = Some(Resume {
        session_id: SESSION.into(),
        usage_totals: baseline,
    });
    let all = run(&fake, request).await;
    assert_eq!(&fake.argv()[fake.argv().len() - 2..], ["--resume", SESSION]);
    let deltas: Vec<&ModelUsage> = all
        .iter()
        .filter_map(|event| match event {
            Event::Usage(delta) => Some(delta),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas,
        [
            &ModelUsage {
                model: Some("claude-haiku-4-5".into()),
                usage: tokens(40, 12, 0, 0, 200),
            },
            &ModelUsage {
                model: Some(OPUS.into()),
                usage: tokens(500, 60, 5000, 0, 25_000),
            },
        ]
    );
    let Some(Event::Finished { usage_totals, .. }) = all.last() else {
        panic!("{all:?}");
    };
    assert_eq!(
        usage_totals,
        &[
            ModelUsage {
                model: Some("claude-haiku-4-5".into()),
                usage: tokens(40, 12, 0, 0, 200),
            },
            ModelUsage {
                model: Some(OPUS.into()),
                usage: tokens(1500, 260, 30_000, 4000, 125_000),
            },
        ]
    );
}

#[tokio::test]
async fn a_subscription_run_that_reports_an_api_key_is_stopped_at_once() {
    let fake = Fake::new("api-key-source");
    let started = Instant::now();
    let all = run(&fake, request(&fake.root())).await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it hangs otherwise"
    );
    assert_eq!(
        all[0],
        Event::SessionStarted {
            session_id: SESSION.into(),
            model: Some(OPUS.into()),
            api_key_source: Some("ANTHROPIC_API_KEY".into()),
        }
    );
    assert!(texts(&all).is_empty(), "nothing after the init: {all:?}");
    let (kind, message) = failure(&all);
    assert_eq!(kind, FailureKind::UnexpectedApiKey);
    assert!(message.contains("\"ANTHROPIC_API_KEY\""), "{message}");
}

// #118: an API key account.

fn api_key_request(cwd: &Path, key: &str) -> RunRequest {
    let mut request = request(cwd);
    request.account = AccountRef {
        id: "anthropic-key".into(),
        credential: Credential::ApiKey(ApiKey::new(key.into())),
    };
    request
}

#[tokio::test]
async fn an_api_key_account_gets_only_its_key_and_reports_it_as_the_source() {
    let fake = Fake::new("api-key-completed");
    let key = "sk-ant-api03-test-key-not-real";
    let all = run(&fake, api_key_request(&fake.root(), key)).await;
    assert_eq!(
        all[0],
        Event::SessionStarted {
            session_id: SESSION.into(),
            model: Some(OPUS.into()),
            api_key_source: Some("ANTHROPIC_API_KEY".into()),
        }
    );
    assert!(
        matches!(outcome(&all), Outcome::Completed { .. }),
        "{all:?}"
    );
    let env = fake.env();
    // Every other inherited credential is scrubbed; ANTHROPIC_API_KEY is Parallax's own value, not
    // the poisoned inherited one INHERITED_CREDENTIALS set.
    for (name, value) in INHERITED_CREDENTIALS {
        let prefix = format!("{name}=");
        let leaked: Vec<&String> = env
            .iter()
            .filter(|var| var.starts_with(&prefix))
            .filter(|var| *name != "ANTHROPIC_API_KEY" || var.ends_with(value))
            .collect();
        assert!(leaked.is_empty(), "{name} leaked: {leaked:?}");
    }
    assert!(env.contains(&format!("ANTHROPIC_API_KEY={key}")), "{env:?}");
    assert!(
        env.contains(&"CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1".to_owned()),
        "{env:?}"
    );
    let serialized = format!("{all:?}");
    assert!(
        !serialized.contains(key),
        "the key must not appear in an event: {serialized}"
    );
}

#[tokio::test]
async fn a_key_account_resolved_from_the_keystore_runs_end_to_end() {
    use parallax_protocol::{AccountId, Provider};

    use crate::backend::key_account;
    use crate::keystore::{KeyStore, MemoryKeyStore};

    let store = MemoryKeyStore::new();
    let account = AccountId::generate();
    let key = "sk-ant-api03-from-the-keystore";
    store.set(account, key).unwrap();
    let credential = key_account::resolve(&store, Provider::Anthropic, account).unwrap();

    let fake = Fake::new("api-key-completed");
    let mut request = request(&fake.root());
    request.account = AccountRef {
        id: account.to_string(),
        credential,
    };
    let all = run(&fake, request).await;
    assert_eq!(
        all[0],
        Event::SessionStarted {
            session_id: SESSION.into(),
            model: Some(OPUS.into()),
            api_key_source: Some("ANTHROPIC_API_KEY".into()),
        }
    );
    assert!(
        matches!(outcome(&all), Outcome::Completed { .. }),
        "{all:?}"
    );
    let env = fake.env();
    assert!(env.contains(&format!("ANTHROPIC_API_KEY={key}")), "{env:?}");
}

#[tokio::test]
async fn an_api_key_run_reporting_a_different_source_fails() {
    let fake = Fake::new("read-only");
    let key = "sk-ant-api03-test-key-not-real";
    let all = run(&fake, api_key_request(&fake.root(), key)).await;
    let (kind, message) = failure(&all);
    assert_eq!(kind, FailureKind::UnexpectedApiKey);
    assert!(
        !message.contains(key),
        "the key must not appear in a failure message: {message}"
    );
}

#[tokio::test]
async fn an_api_key_never_reaches_tracing_output() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let capture = Capture::default();
    let for_writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || for_writer.clone())
        .finish();
    let guard = tracing::subscriber::set_default(subscriber);
    let key = "sk-ant-api03-test-key-not-real";

    // A completed run and a failed one (the mismatch check), so both outcomes are covered.
    let completed = Fake::new("api-key-completed");
    let completed_request = api_key_request(&completed.root(), key);
    assert!(
        !format!("{completed_request:?}").contains(key),
        "the key must not appear in a request's Debug output"
    );
    tracing::info!("parallax-test-sentinel: starting the completed run");
    let all = run(&completed, completed_request).await;
    assert!(
        matches!(outcome(&all), Outcome::Completed { .. }),
        "{all:?}"
    );

    let mismatched = Fake::new("read-only");
    tracing::info!("parallax-test-sentinel: starting the mismatched run");
    let all = run(&mismatched, api_key_request(&mismatched.root(), key)).await;
    assert_eq!(failure(&all).0, FailureKind::UnexpectedApiKey);

    drop(guard);
    let logged = String::from_utf8_lossy(&capture.0.lock().unwrap()).into_owned();
    assert!(
        logged.contains("parallax-test-sentinel: starting the mismatched run"),
        "the capture never saw anything, so it can't prove the key's absence: {logged:?}"
    );
    assert!(
        !logged.contains(key),
        "the key leaked into tracing output: {logged}"
    );
}

// The fake CLI's own scrubbing of its child's environment stands in for the real CLI's, which
// is documented (0004 [16]) but not something plxd can verify directly: this proves plxd sets
// CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1 and that a CLI honoring it keeps the key from a subprocess.
#[tokio::test]
async fn a_tool_subprocess_the_cli_spawns_never_sees_the_key() {
    let fake = Fake::new("subprocess-env");
    let key = "sk-ant-api03-test-key-not-real";
    run(&fake, api_key_request(&fake.root(), key)).await;
    assert!(
        fake.env().contains(&format!("ANTHROPIC_API_KEY={key}")),
        "the CLI itself must have had the key, or this test proves nothing: {:?}",
        fake.env()
    );
    let child_env: Vec<String> = fake
        .recorded("child-env")
        .lines()
        .map(str::to_owned)
        .collect();
    assert!(
        !child_env.iter().any(|var| var.contains(key)),
        "{child_env:?}"
    );
    assert!(
        child_env
            .iter()
            .any(|var| var.starts_with("CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=")),
        "the child should still see the flag itself: {child_env:?}"
    );
}

#[tokio::test]
async fn a_no_write_run_offered_write_tools_is_stopped() {
    let fake = Fake::new("write-tools");
    let all = run(&fake, request(&fake.root())).await;
    let (kind, message) = failure(&all);
    assert_eq!(kind, FailureKind::PolicyViolation);
    assert!(message.ends_with("Bash, Edit"), "{message}");
    assert!(texts(&all).is_empty());
}

#[tokio::test]
async fn malformed_lines_warn_and_unknown_types_are_skipped_quietly() {
    let fake = Fake::new("malformed");
    let all = run(&fake, request(&fake.root())).await;
    assert_eq!(
        warnings(&all),
        [
            WarningKind::MalformedLine,
            WarningKind::MalformedLine,
            WarningKind::MalformedLine,
            WarningKind::MalformedLine,
        ]
    );
    assert_eq!(texts(&all), ["Still here."]);
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("Still here.".into())
        }
    );
}

#[tokio::test]
async fn a_follow_up_during_a_turn_that_the_cli_folds_in_finishes_with_it() {
    let fake = Fake::new("follow-up-folded");
    let Started { run, mut events } = launch(&fake.backend, request(&fake.root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    assert!(matches!(next(&mut events).await, Event::Text { .. }));
    let follow_up = FollowUp {
        turn_id: turn(TURN_2),
        text: "Fix the tests too.".into(),
        images: Vec::new(),
    };
    run.send(follow_up.clone()).unwrap();
    run.send(follow_up).unwrap();
    let all = rest(&mut events).await;
    let turns: Vec<&Event> = all
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::TurnStarted { .. } | Event::TurnFinished { .. } | Event::Text { .. }
            )
        })
        .collect();
    let done = Some("Parser and tests done.".to_owned());
    assert_eq!(
        turns,
        [
            &Event::TurnStarted {
                turn_id: Some(turn(TURN_2))
            },
            &Event::Text {
                message_id: Some("msg_01Ff2".into()),
                text: "And the tests, as you asked.".into()
            },
            &Event::TurnFinished {
                turn_id: Some(turn(TURN_1)),
                result: done.clone()
            },
            &Event::TurnFinished {
                turn_id: Some(turn(TURN_2)),
                result: done
            },
        ]
    );
    let stdin = fake.stdin();
    assert_eq!(stdin.len(), 2, "sent once: {stdin:?}");
    assert_eq!(stdin[1]["uuid"], TURN_2);
    assert_eq!(stdin[1]["message"]["content"], "Fix the tests too.");
    assert!(matches!(outcome(&all), Outcome::Completed { .. }));
    assert_eq!(
        run.send(FollowUp {
            turn_id: TurnId::generate(),
            text: "too late".into(),
            images: Vec::new(),
        }),
        Err(SendError::Finished)
    );
}

#[tokio::test]
async fn a_follow_up_can_be_its_own_turn_and_stdin_waits_for_it() {
    let fake = Fake::new("follow-up-turns");
    let Started { run, mut events } = launch(&fake.backend, request(&fake.root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    assert!(matches!(next(&mut events).await, Event::Text { .. }));
    run.send(FollowUp {
        turn_id: turn(TURN_2),
        text: "And now the docs.".into(),
        images: Vec::new(),
    })
    .unwrap();
    let all = rest(&mut events).await;
    let sessions = all
        .iter()
        .filter(|event| matches!(event, Event::SessionStarted { .. }))
        .count();
    assert_eq!(
        sessions, 0,
        "the second turn's init repeats the same session"
    );
    let turns: Vec<&Event> = all
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::TurnStarted { .. } | Event::TurnFinished { .. }
            )
        })
        .collect();
    assert_eq!(
        turns,
        [
            &Event::TurnStarted {
                turn_id: Some(turn(TURN_2))
            },
            &Event::TurnFinished {
                turn_id: Some(turn(TURN_1)),
                result: Some("First answer.".into())
            },
            &Event::TurnFinished {
                turn_id: Some(turn(TURN_2)),
                result: Some("Second answer.".into())
            },
        ]
    );
    let usage: u64 = all
        .iter()
        .filter_map(|event| match event {
            Event::Usage(delta) => Some(delta.usage.output_tokens),
            _ => None,
        })
        .sum();
    assert_eq!(usage, 220, "cumulative totals across turns become deltas");
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("Second answer.".into())
        }
    );
}

#[tokio::test]
async fn images_go_before_the_text_as_base64_blocks_and_a_message_of_images_alone_has_no_text() {
    let png = PromptImage {
        media_type: ImageMediaType::Png,
        data: "iVBORw0KGgo=".into(),
    };
    let gif = PromptImage {
        media_type: ImageMediaType::Gif,
        data: "R0lGODlh".into(),
    };
    let fake = Fake::new("follow-up-turns");
    let request = RunRequest {
        images: vec![png, gif.clone()],
        ..request(&fake.root())
    };
    let Started { run, mut events } = launch(&fake.backend, request).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    run.send(FollowUp {
        turn_id: turn(TURN_2),
        text: String::new(),
        images: vec![gif],
    })
    .unwrap();
    rest(&mut events).await;
    let block = |media_type: &str, data: &str| {
        serde_json::json!({
            "type": "image",
            "source": {"type": "base64", "media_type": media_type, "data": data},
        })
    };
    let text = |text: &str| serde_json::json!({"type": "text", "text": text});
    let stdin = fake.stdin();
    assert_eq!(
        stdin[0]["message"]["content"],
        serde_json::json!([
            block("image/png", "iVBORw0KGgo="),
            block("image/gif", "R0lGODlh"),
            text("Summarize the README.\nKeep it short."),
        ])
    );
    assert_eq!(
        stdin[1]["message"]["content"],
        serde_json::json!([block("image/gif", "R0lGODlh")])
    );
}

#[tokio::test]
async fn a_resumed_session_starts_on_images_alone() {
    let fake = Fake::new("resume");
    let request = RunRequest {
        prompt: String::new(),
        images: vec![PromptImage {
            media_type: ImageMediaType::Png,
            data: "iVBORw0KGgo=".into(),
        }],
        resume: Some(Resume {
            session_id: SESSION.into(),
            usage_totals: Vec::new(),
        }),
        ..request(&fake.root())
    };
    run(&fake, request).await;
    assert_eq!(
        fake.stdin()[0]["message"]["content"],
        serde_json::json!([{
            "type": "image",
            "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="},
        }])
    );
}

#[tokio::test]
async fn cancel_interrupts_the_cli_with_sigint() {
    let fake = Fake::new("cancel");
    let Started { run, mut events } = launch(&fake.backend, request(&fake.root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    // The fake CLI prints `@trap-armed` right after installing its SIGINT trap (fake-claude.sh),
    // which the translator reports as a malformed line. Waiting for it here is a deterministic
    // handshake: cancel() below can never race the trap's own installation (#149), unlike waiting
    // for a wall-clock margin. The fake then blocks reading stdin, which cancel closes after the
    // SIGINT, so a trap that bash left pending still runs at EOF (RYA-120, cancel.jsonl).
    assert!(matches!(
        next(&mut events).await,
        Event::Warning {
            warning: WarningKind::MalformedLine,
            ..
        }
    ));
    assert!(matches!(next(&mut events).await, Event::Text { .. }));
    let started = Instant::now();
    run.cancel();
    run.cancel();
    let all = rest(&mut events).await;
    assert_eq!(
        all,
        [Event::Finished {
            outcome: Outcome::Cancelled,
            usage_totals: Vec::new()
        }]
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(fake.recorded("signals"), "SIGINT\n");
}

#[tokio::test]
async fn cancel_escalates_to_sigkill_after_the_grace_period() {
    let fake = Fake::new("stubborn");
    let backend = fake.backend.clone().with_cancel_policy(CancelPolicy {
        grace: Duration::from_millis(300),
        ..CancelPolicy::default()
    });
    let Started { run, mut events } = launch(&backend, request(&fake.root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    assert!(matches!(next(&mut events).await, Event::Text { .. }));
    let started = Instant::now();
    run.cancel();
    let all = rest(&mut events).await;
    assert_eq!(outcome(&all), &Outcome::Cancelled);
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(300) && elapsed < Duration::from_secs(5),
        "{elapsed:?}"
    );
}

#[tokio::test]
async fn bad_requests_are_refused_before_spawning() {
    let fake = Fake::new("read-only");
    let cwd = fake.root();
    let mut empty = request(&cwd);
    empty.prompt.clear();
    let mut flag_model = request(&cwd);
    flag_model.model = Some("--dangerously-skip-permissions".into());
    let mut spaced_resume = request(&cwd);
    spaced_resume.resume = Some(Resume::new("a session"));
    for bad in [empty, flag_model, spaced_resume] {
        assert!(
            matches!(fake.backend.start(bad), Err(StartError::Invalid(_))),
            "accepted"
        );
    }
    let missing = fake.backend.clone().with_program("claude-not-installed");
    assert!(matches!(
        missing.start(request(&cwd)),
        Err(StartError::Spawn(SpawnError::NotFound { .. }))
    ));
    assert_eq!(fake.argv(), Vec::<String>::new(), "nothing ran");
}

#[test]
fn an_init_that_does_not_say_where_its_credentials_came_from_is_refused() {
    let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none");
    let init = br#"{"type":"system","subtype":"init","session_id":"s","tools":["Bash"]}"#;
    let steps = translator.line(init);
    assert!(
        matches!(
            steps.last(),
            Some(Step::Violation(failure)) if failure.failure == FailureKind::UnexpectedApiKey
        ),
        "{steps:?}"
    );
}

#[test]
fn command_lifecycle_messages_are_skipped_without_a_notice() {
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none");
    // Shape from the claude-codes changelog, not recorded from a run.
    let line = br#"{"type":"command_lifecycle","command_uuid":"01997e2a-4c3b-7d10-8a2e-5f6b7c8d9e01","state":"started","session_id":"5b1e3c9a-8f2d-4c6e-9a1b-3d7f0e2c4a68"}"#;
    assert_eq!(translator.line(line), []);
}

#[test]
fn output_before_the_init_is_refused() {
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none");
    let assistant =
        br#"{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"hi"}]}}"#;
    assert!(matches!(
        translator.line(assistant).as_slice(),
        [Step::Violation(failure)] if failure.failure == FailureKind::UnexpectedApiKey
    ));
}

fn init_line(tools: &str) -> Vec<u8> {
    init_with_version(tools, "2.1.281")
}

/// An init line in a worker's permission mode. No-write runs don't check theirs.
fn init_with_version(tools: &str, version: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"system","subtype":"init","session_id":"s","apiKeySource":"none","claude_code_version":"{version}","permissionMode":"acceptEdits","tools":{tools}}}"#
    )
    .into_bytes()
}

#[test]
fn a_worker_on_a_claude_code_too_old_to_sandbox_it_is_stopped() {
    for (reported, refused) in [
        ("2.1.247", true),
        ("2.0.999", true),
        ("1.9.300", true),
        ("not-a-version", true),
        ("2.1.248", false),
        ("2.1.281", false),
        ("2.2.0", false),
        ("10.0.0-beta.1", false),
    ] {
        let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none");
        let steps = translator.line(&init_with_version(r#"["Bash"]"#, reported));
        let expected = refused.then_some(FailureKind::PolicyViolation);
        assert_eq!(violation_kind(&steps), expected, "{reported}");
    }
    let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none");
    let unversioned =
        br#"{"type":"system","subtype":"init","session_id":"s","apiKeySource":"none","tools":["Bash"]}"#;
    assert_eq!(
        violation_kind(&translator.line(unversioned)),
        Some(FailureKind::PolicyViolation)
    );
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none");
    let old_reader = init_with_version(r#"["Read"]"#, "2.1.200");
    assert_eq!(
        violation_kind(&translator.line(&old_reader)),
        None,
        "no-write runs have no floor"
    );
}

#[test]
fn a_worker_must_report_the_permission_mode_it_asked_for() {
    let init = |mode: &str| {
        format!(
            r#"{{"type":"system","subtype":"init","session_id":"s","apiKeySource":"none","claude_code_version":"2.1.283","tools":["Read"]{mode}}}"#
        )
    };
    for (mode, refused) in [
        (r#","permissionMode":"acceptEdits""#, false),
        (r#","permissionMode":"default""#, true),
        (r#","permissionMode":"bypassPermissions""#, true),
        ("", true),
    ] {
        let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none");
        let steps = translator.line(init(mode).as_bytes());
        let expected = refused.then_some(FailureKind::PolicyViolation);
        assert_eq!(violation_kind(&steps), expected, "{mode}");
    }
    // A plan worker (RYA-97) must report plan mode, and scrub mode's "default" still fails it.
    for (mode, refused) in [
        (r#","permissionMode":"plan""#, false),
        (r#","permissionMode":"acceptEdits""#, true),
        (r#","permissionMode":"default""#, true),
    ] {
        let mut translator =
            Translator::new(ToolPolicy::WorkspaceWrite, "none").with_permission_mode("plan");
        let steps = translator.line(init(mode).as_bytes());
        let expected = refused.then_some(FailureKind::PolicyViolation);
        assert_eq!(violation_kind(&steps), expected, "{mode}");
    }
    // plxd sets CLAUDE_CODE_SUBPROCESS_ENV_SCRUB for a no-write run, which forces "default".
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none");
    let steps = translator.line(init(r#","permissionMode":"default""#).as_bytes());
    assert_eq!(violation_kind(&steps), None);
}

fn violation_kind(steps: &[Step]) -> Option<FailureKind> {
    steps.iter().find_map(|step| match step {
        Step::Violation(failure) => Some(failure.failure),
        _ => None,
    })
}

#[test]
fn a_worker_run_allows_only_the_worker_tools() {
    // An older Claude Code, or `CLAUDE_CODE_ENABLE_TASKS=false`, lists `TodoWrite`; 2.1.283 lists
    // the task tools in its place (RYA-248).
    for todo_tools in [
        r#""TodoWrite""#,
        r#""TaskCreate","TaskGet","TaskList","TaskUpdate""#,
    ] {
        let allowed = init_line(&format!(
            r#"["Read","Edit","Write","Glob","Grep","NotebookEdit","Bash","WebFetch","WebSearch",{todo_tools},"EndConversation"]"#
        ));
        let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none");
        assert_eq!(
            violation_kind(&translator.line(&allowed)),
            None,
            "{todo_tools}"
        );
    }
    for extra in [
        r#"["Bash","Monitor"]"#,
        r#"["Bash","Task"]"#,
        r#"["Bash","Agent"]"#,
        r#"["Bash","Skill"]"#,
        r#"["Bash","mcp__github__create_issue"]"#,
        // Background tasks' tools, which share the task tools' prefix but not their purpose.
        r#"["TaskCreate","TaskStop"]"#,
        r#"["TaskCreate","TaskOutput"]"#,
    ] {
        let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none");
        assert_eq!(
            violation_kind(&translator.line(&init_line(extra))),
            Some(FailureKind::PolicyViolation),
            "{extra}"
        );
    }
}

/// RYA-248: a worker on Claude Code 2.1.283 plans with the task tools, which its `--tools` names
/// and its init lists in place of `TodoWrite`. Each call, and its result's text, which says the
/// task's id, reach the app as they are; it builds the plan from them, so plxd makes no checklist.
#[tokio::test]
async fn a_worker_plans_with_claude_code_s_task_tools() {
    let fake = Fake::new("worker-tasks");
    let mut request = request(&fake.root());
    request.policy = ToolPolicy::WorkspaceWrite;
    request.sandbox = Some(worker_sandbox(&fake.root()));
    let all = run(&fake, request).await;
    let argv = fake.argv();
    let tools = argv.iter().position(|arg| arg == "--tools").unwrap();
    assert_eq!(argv[tools + 1], WORKER_TOOL_LIST);

    let calls: Vec<(&str, Value)> = all
        .iter()
        .filter_map(|event| match event {
            Event::ToolCall { name, input, .. } => Some((name.as_str(), input.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        [
            (
                "TaskCreate",
                serde_json::json!({
                    "subject": "Add tests",
                    "description": "Cover the parser",
                    "activeForm": "Adding tests",
                })
            ),
            (
                "TaskUpdate",
                serde_json::json!({"taskId": "1", "status": "in_progress"})
            ),
            (
                "TaskUpdate",
                serde_json::json!({"taskId": "1", "status": "completed"})
            ),
        ]
    );
    let results: Vec<(&str, ToolStatus, Option<&str>)> = all
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult {
                call_id,
                status,
                output,
            } => Some((call_id.as_str(), *status, output.as_deref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [
            (
                "toolu_01Task00",
                ToolStatus::Ok,
                Some("Task #1 created successfully: Add tests")
            ),
            (
                "toolu_01Task01",
                ToolStatus::Ok,
                Some("Updated task #1 status")
            ),
            (
                "toolu_01Task02",
                ToolStatus::Ok,
                Some("Updated task #1 status")
            ),
        ]
    );
    assert!(
        !all.iter()
            .any(|event| matches!(event, Event::TodoList { .. })),
        "{all:?}"
    );
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("done".into())
        }
    );
}

/// RYA-249: a coordinator and a bypass worker have no `--tools`, so they name the todo tools in
/// `--allowedTools`, which turns them on for any model, in every mode. A coordinator's list
/// starts with plxd's own tools. Any other worker names them in `--tools` and gets no
/// allowlist, and a plain no-write run keeps 0004's flags.
#[test]
fn a_coordinator_and_a_bypass_worker_allow_the_todo_tools_in_every_mode() {
    let todo = "TodoWrite,TaskCreate,TaskGet,TaskList,TaskUpdate";
    assert_eq!(TODO_TOOLS.join(","), todo);
    assert!(WORKER_TOOLS.ends_with(TODO_TOOLS), "{WORKER_TOOLS:?}");
    let cwd = Path::new("/Users/u/wt");
    let mut worker = request(cwd);
    worker.policy = ToolPolicy::WorkspaceWrite;
    worker.sandbox = Some(worker_sandbox(cwd));
    let allowed = |request: &RunRequest| -> Vec<String> {
        let args: Vec<String> = super::arguments(request)
            .unwrap()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        args.iter()
            .enumerate()
            .filter(|(_, arg)| *arg == "--allowedTools")
            .map(|(at, _)| args[at + 1].clone())
            .collect()
    };
    let coordinator_list = format!("{},{todo}", mcp::ALLOWED_TOOLS.join(","));
    for permission in [
        None,
        Some(AgentPermission::Edit),
        Some(AgentPermission::Auto),
        Some(AgentPermission::Manual),
        Some(AgentPermission::Plan),
        Some(AgentPermission::Bypass),
    ] {
        for approvals in [false, true] {
            let run = RunRequest {
                permission,
                approvals,
                ..coordinator(cwd)
            };
            assert_eq!(allowed(&run), [coordinator_list.as_str()], "{permission:?}");
            let run = RunRequest {
                permission,
                approvals,
                ..worker.clone()
            };
            let expected: &[&str] = if permission == Some(AgentPermission::Bypass) {
                &[todo]
            } else {
                &[]
            };
            assert_eq!(allowed(&run), expected, "{permission:?}");
        }
    }
    assert_eq!(allowed(&request(cwd)), Vec::<String>::new());
}

/// RYA-249: a coordinator on Claude Code 2.1.283 and a model outside its built-in list plans with
/// the task tools, which its `--allowedTools` turns on and its init lists. Each call and its
/// result reach the app as they are, as a worker's do (RYA-248).
#[tokio::test]
async fn a_coordinator_plans_with_claude_code_s_task_tools_on_any_model() {
    let fake = Fake::new("coordinator-tasks");
    let request = RunRequest {
        model: Some("claude-opus-5-5".into()),
        ..coordinator(&fake.root())
    };
    let all = run(&fake, request).await;
    let argv = fake.argv();
    let allowed = argv.iter().position(|arg| arg == "--allowedTools").unwrap();
    assert!(
        argv[allowed + 1].ends_with(",TodoWrite,TaskCreate,TaskGet,TaskList,TaskUpdate"),
        "{argv:?}"
    );
    assert!(!argv.iter().any(|arg| arg == "--tools"), "{argv:?}");

    let calls: Vec<(&str, Value)> = all
        .iter()
        .filter_map(|event| match event {
            Event::ToolCall { name, input, .. } => Some((name.as_str(), input.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        calls,
        [
            (
                "TaskCreate",
                serde_json::json!({
                    "subject": "Add tests",
                    "description": "Cover the parser",
                    "activeForm": "Adding tests",
                })
            ),
            (
                "TaskUpdate",
                serde_json::json!({"taskId": "1", "status": "in_progress"})
            ),
            ("TaskList", serde_json::json!({})),
        ]
    );
    let results: Vec<(&str, ToolStatus, Option<&str>)> = all
        .iter()
        .filter_map(|event| match event {
            Event::ToolResult {
                call_id,
                status,
                output,
            } => Some((call_id.as_str(), *status, output.as_deref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        results,
        [
            (
                "toolu_01ParallaxProbe",
                ToolStatus::Ok,
                Some("Task #1 created successfully: Add tests")
            ),
            (
                "toolu_01ParallaxProbe1",
                ToolStatus::Ok,
                Some("Updated task #1 status")
            ),
            (
                "toolu_01ParallaxProbe2",
                ToolStatus::Ok,
                Some("#1 [in_progress] Add tests")
            ),
        ]
    );
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("done".into())
        }
    );
}

#[test]
fn a_no_write_run_allows_only_the_read_tools() {
    let allowed = init_line(r#"["Read","Glob","Grep","EndConversation"]"#);
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none");
    assert_eq!(violation_kind(&translator.line(&allowed)), None);
    for extra in [
        r#"["Read","WebFetch"]"#,
        r#"["Read","Task"]"#,
        r#"["Read","mcp__github__create_issue"]"#,
        r#"["Read",7]"#,
    ] {
        let mut translator = Translator::new(ToolPolicy::NoWrite, "none");
        assert_eq!(
            violation_kind(&translator.line(&init_line(extra))),
            Some(FailureKind::PolicyViolation),
            "{extra}"
        );
    }
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none");
    let no_tools = br#"{"type":"system","subtype":"init","session_id":"s","apiKeySource":"none"}"#;
    assert_eq!(
        violation_kind(&translator.line(no_tools)),
        Some(FailureKind::PolicyViolation)
    );
}

/// RYA-276, 0034: a thread is full Claude Code in every mode, as a bypass worker is: no
/// `--restricted`, `--tools`, `--strict-mcp-config`, or sandbox settings, so the user's settings,
/// skills, and MCP servers load. It asks plxd in Accept Edits too, since nothing sandboxes its
/// commands, and in Plan it has `ExitPlanMode` without a `--tools` list. Its init may list any
/// tool. A worker keeps its sandbox, and so does a thread whose client can't answer requests.
#[test]
fn a_thread_is_full_claude_code_in_every_mode() {
    let cwd = Path::new("/Users/u/wt");
    let mut thread = request(cwd);
    thread.policy = ToolPolicy::WorkspaceWrite;
    thread.sandbox = Some(worker_sandbox(cwd));
    thread.approvals = true;
    thread.thread = true;
    for permission in [
        None,
        Some(AgentPermission::Auto),
        Some(AgentPermission::Manual),
        Some(AgentPermission::Edit),
        Some(AgentPermission::Plan),
        Some(AgentPermission::Bypass),
    ] {
        let request = RunRequest {
            permission,
            ..thread.clone()
        };
        let args: Vec<String> = super::arguments(&request)
            .unwrap()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        for flag in ["--restricted", "--tools", "--strict-mcp-config"] {
            assert!(
                !args.contains(&flag.to_owned()),
                "{permission:?} {flag}: {args:?}"
            );
        }
        let settings = args.iter().position(|arg| arg == "--settings").unwrap();
        assert_eq!(
            args[settings + 1],
            r#"{"env":{"CLAUDE_CODE_TASK_LIST_ID":""}}"#
        );
        let asks = permission != Some(AgentPermission::Bypass);
        assert_eq!(super::prompts(&request), asks, "{permission:?}");
        assert!(!super::hands_over_plans(&request));
    }
    let unanswered = RunRequest {
        approvals: false,
        ..thread.clone()
    };
    assert!(
        super::arguments(&unanswered)
            .unwrap()
            .contains(&"--restricted".into())
    );
    let worker = RunRequest {
        thread: false,
        ..thread
    };
    assert!(
        super::arguments(&worker)
            .unwrap()
            .contains(&"--restricted".into())
    );
    assert!(
        !super::prompts(&worker),
        "a sandboxed worker in Accept Edits never asks"
    );

    let loaded = init_line(r#"["Read","Bash","Skill","Task","mcp__linear__list_issues"]"#);
    let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none").with_thread(true);
    assert_eq!(violation_kind(&translator.line(&loaded)), None);
    let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none");
    assert_eq!(
        violation_kind(&translator.line(&loaded)),
        Some(FailureKind::PolicyViolation)
    );
}

/// 0027: a coordinator and a bypass worker are full Claude Code, so their init may list any
/// tool, another MCP server's included, in the mode they asked for. Without plxd's tools a
/// no-write run keeps its read tools.
#[test]
fn a_coordinator_and_a_bypass_worker_allow_any_tool_in_the_mode_they_asked_for() {
    let tools =
        r#"["Read","Edit","Bash","Task","mcp__plxd__spawn_agent","mcp__linear__list_issues"]"#;
    let loaded = init_line(tools);
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none").with_coordinator_tools(true);
    assert_eq!(violation_kind(&translator.line(&loaded)), None);
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none")
        .with_coordinator_tools(true)
        .with_permission_mode("bypassPermissions");
    assert_eq!(
        violation_kind(&translator.line(&loaded)),
        Some(FailureKind::PolicyViolation),
        "it reported acceptEdits"
    );
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none");
    assert_eq!(
        violation_kind(&translator.line(&loaded)),
        Some(FailureKind::PolicyViolation),
        "without plxd's tools it isn't a coordinator"
    );

    let bypass = String::from_utf8(loaded)
        .unwrap()
        .replace("acceptEdits", "bypassPermissions");
    let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none")
        .with_permission_mode("bypassPermissions");
    assert_eq!(violation_kind(&translator.line(bypass.as_bytes())), None);
}

fn failed_result() -> &'static [u8] {
    br#"{"type":"result","subtype":"success","is_error":true,"result":"API Error"}"#
}

fn last_failure(translator: &Translator) -> FailureKind {
    translator.last_failure.as_ref().unwrap().failure
}

#[test]
fn retries_and_rejected_limits_name_a_failed_turn_s_kind_until_it_ends() {
    let mut translator = Translator::new(ToolPolicy::WorkspaceWrite, "none");
    translator.line(&init_line(r#"["Read"]"#));
    for (error, kind) in [
        ("rate_limit", FailureKind::RateLimited),
        ("authentication_failed", FailureKind::NotSignedIn),
        ("overloaded", FailureKind::VendorError),
    ] {
        let retry = format!(r#"{{"type":"system","subtype":"api_retry","error":"{error}"}}"#);
        translator.line(retry.as_bytes());
        translator.line(failed_result());
        assert_eq!(last_failure(&translator), kind, "{error}");
    }

    let rejected = br#"{"type":"rate_limit_event","rate_limit_info":{"status":"rejected","rateLimitType":"five_hour"}}"#;
    translator.line(rejected);
    translator.line(failed_result());
    assert_eq!(last_failure(&translator), FailureKind::RateLimited);
    translator.line(failed_result());
    assert_eq!(
        last_failure(&translator),
        FailureKind::VendorError,
        "the next turn starts clean"
    );

    translator.line(rejected);
    let max_turns = br#"{"type":"result","subtype":"error_max_turns","is_error":true}"#;
    translator.line(max_turns);
    assert_eq!(
        last_failure(&translator),
        FailureKind::VendorError,
        "a limit the run set says nothing about the account"
    );
}

/// A coordinator's request at `cwd`, whose plxd tools make it full Claude Code (0027).
fn coordinator(cwd: &Path) -> RunRequest {
    RunRequest {
        coordinator_tools: Some(CoordinatorTools {
            program: PathBuf::from("/Applications/Parallax.app/Contents/Resources/plxd"),
            data_dir: cwd.join("data"),
            project: ProjectId::generate(),
            thread: CoordinatorThreadId::generate(),
        }),
        ..request(cwd)
    }
}

/// RYA-222: Manual, Auto, and Plan ask plxd over stdio, right after the mode, for a worker and a
/// coordinator alike, when the client answers. Without `approvals`, every mode runs as before, as
/// do Accept Edits and Bypass Permissions, and a plain no-write run never asks.
#[test]
fn only_the_modes_that_prompt_ask_plxd_over_stdio_and_only_with_approvals() {
    let cwd = Path::new("/Users/u/wt");
    let mut worker = request(cwd);
    worker.policy = ToolPolicy::WorkspaceWrite;
    worker.sandbox = Some(worker_sandbox(cwd));
    for (base, approvals) in [
        (worker.clone(), true),
        (coordinator(cwd), true),
        (worker, false),
        (coordinator(cwd), false),
    ] {
        for (permission, prompting) in [
            (None, false),
            (Some(AgentPermission::Edit), false),
            (Some(AgentPermission::Bypass), false),
            (Some(AgentPermission::Manual), true),
            (Some(AgentPermission::Auto), true),
            (Some(AgentPermission::Plan), true),
        ] {
            let expected = prompting && approvals;
            let request = RunRequest {
                permission,
                approvals,
                ..base.clone()
            };
            let args: Vec<String> = super::arguments(&request)
                .unwrap()
                .into_iter()
                .map(|arg| arg.into_string().unwrap())
                .collect();
            let mode = args.iter().position(|arg| arg == "--permission-mode");
            let prompt_tool = args.iter().position(|arg| arg == PROMPT_TOOL_ARGS[0]);
            assert_eq!(super::prompts(&request), expected, "{permission:?}");
            if expected {
                assert_eq!(prompt_tool, mode.map(|mode| mode + 2), "{args:?}");
                assert_eq!(args[prompt_tool.unwrap() + 1], PROMPT_TOOL_ARGS[1]);
            } else {
                assert_eq!(prompt_tool, None, "{args:?}");
            }
            // The prompt channel, and a plan worker's `ExitPlanMode` (RYA-243), are the only
            // arguments `approvals` changes.
            if !approvals {
                let mut asking: Vec<String> = super::arguments(&RunRequest {
                    approvals: true,
                    ..request.clone()
                })
                .unwrap()
                .into_iter()
                .map(|arg| arg.into_string().unwrap())
                .collect();
                if let Some(at) = asking.iter().position(|arg| arg == PROMPT_TOOL_ARGS[0]) {
                    asking.drain(at..at + PROMPT_TOOL_ARGS.len());
                }
                if let Some(tools) = asking
                    .iter_mut()
                    .find(|arg| arg.as_str() == PLAN_WORKER_TOOL_LIST)
                {
                    WORKER_TOOL_LIST.clone_into(tools);
                }
                assert_eq!(args, asking, "{permission:?}");
            }
        }
    }
    let plain = RunRequest {
        approvals: true,
        ..request(cwd)
    };
    assert!(!super::prompts(&plain));
    assert!(
        !super::arguments(&plain)
            .unwrap()
            .contains(&PROMPT_TOOL_ARGS[0].into())
    );
}

/// RYA-243: a worker or a thread in Plan whose client answers gets `ExitPlanMode` in `--tools`,
/// so it can hand its plan over. Every other mode, and Plan without `approvals`, keeps 0013's
/// tools exactly; a bypass worker names none, and a coordinator never has the list.
#[test]
fn only_a_plan_worker_that_asks_plxd_gets_exit_plan_mode() {
    let mut plan_tools = WORKER_TOOLS.to_vec();
    plan_tools.push(EXIT_PLAN_MODE);
    assert_eq!(PLAN_WORKER_TOOL_LIST, plan_tools.join(","));
    let mut plan_args = WORKSPACE_WRITE_ARGS.to_vec();
    plan_args[2] = PLAN_WORKER_TOOL_LIST;
    assert_eq!(
        PLAN_WORKSPACE_WRITE_ARGS, plan_args,
        "only the tools differ"
    );

    let cwd = Path::new("/Users/u/wt");
    let mut worker = request(cwd);
    worker.policy = ToolPolicy::WorkspaceWrite;
    worker.sandbox = Some(worker_sandbox(cwd));
    let tools = |request: &RunRequest| {
        let args: Vec<String> = super::arguments(request)
            .unwrap()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        let at = args.iter().position(|arg| arg == "--tools");
        at.map(|at| args[at + 1].clone())
    };
    for approvals in [false, true] {
        for permission in [
            None,
            Some(AgentPermission::Edit),
            Some(AgentPermission::Auto),
            Some(AgentPermission::Manual),
            Some(AgentPermission::Plan),
            Some(AgentPermission::Bypass),
        ] {
            let request = RunRequest {
                permission,
                approvals,
                ..worker.clone()
            };
            let hands_over = approvals && permission == Some(AgentPermission::Plan);
            let expected = match permission {
                Some(AgentPermission::Bypass) => None,
                _ if hands_over => Some(PLAN_WORKER_TOOL_LIST),
                _ => Some(WORKER_TOOL_LIST),
            };
            let context = format!("{permission:?}, approvals {approvals}");
            assert_eq!(super::hands_over_plans(&request), hands_over, "{context}");
            assert_eq!(tools(&request).as_deref(), expected, "{context}");

            let coordinator_run = RunRequest {
                permission,
                approvals,
                ..coordinator(cwd)
            };
            assert!(!super::hands_over_plans(&coordinator_run), "{context}");
            assert_eq!(tools(&coordinator_run), None, "{context}");
        }
    }
    let plain = RunRequest {
        approvals: true,
        ..request(cwd)
    };
    assert!(!super::hands_over_plans(&plain));
    assert_eq!(tools(&plain).as_deref(), Some(NO_WRITE_ARGS[1]));
}

/// RYA-222: in Manual, Claude Code's `can_use_tool` becomes a permission request, and an allow
/// goes back on stdin as the Agent SDK writes it: the input it asked with, and with `always`, only
/// its allow rules, for the session. Its other suggestions are dropped.
#[tokio::test]
async fn an_allowed_permission_request_answers_the_cli_on_stdin() {
    let fake = Fake::new("approval");
    let request = RunRequest {
        permission: Some(AgentPermission::Manual),
        approvals: true,
        ..coordinator(&fake.root())
    };
    let Started { run, mut events } = launch(&fake.backend, request).await;
    let asked = until_asked(&mut events).await;
    assert_eq!(asked.tool_name, "Bash");
    assert_eq!(
        asked.input,
        serde_json::json!({"command": "pnpm test", "description": "Run the tests"})
    );
    assert_eq!(asked.call_id.as_deref(), Some("toolu_01Ap1"));
    assert_eq!(asked.always_allow, ["Bash(pnpm test:*)"]);
    assert_eq!(
        asked.reason.as_deref(),
        Some("Current permission mode (Default) requires approval for this Bash command"),
        "terminal escapes are removed"
    );
    assert!(!asked.interactive);
    run.answer(Answer {
        approval_id: asked.approval_id,
        decision: Decision::Allow {
            input: None,
            always: true,
        },
    })
    .unwrap();
    let all = rest(&mut events).await;
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("Ran the tests.".into())
        }
    );
    let argv = fake.argv();
    let mode = argv
        .iter()
        .position(|arg| arg == "--permission-mode")
        .unwrap();
    assert_eq!(
        argv[mode..mode + 4],
        [
            "--permission-mode",
            "default",
            "--permission-prompt-tool",
            "stdio"
        ]
    );
    assert_eq!(
        fake.stdin()[1],
        serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "success",
                "request_id": "req-ap-1",
                "response": {
                    "behavior": "allow",
                    "updatedInput": {"command": "pnpm test", "description": "Run the tests"},
                    "updatedPermissions": [{
                        "type": "addRules",
                        "rules": [{"toolName": "Bash", "ruleContent": "pnpm test:*"}],
                        "behavior": "allow",
                        "destination": "session",
                    }],
                    "toolUseID": "toolu_01Ap1",
                },
            },
        })
    );
}

/// RYA-222: a denial goes back with its message, and an edited input replaces the one asked with.
#[tokio::test]
async fn a_denied_or_edited_request_answers_with_what_the_user_said() {
    for (decision, expected) in [
        (
            Decision::Deny {
                message: "Not the whole suite.".into(),
                interrupt: false,
            },
            serde_json::json!({
                "behavior": "deny",
                "message": "Not the whole suite.",
                "interrupt": false,
                "toolUseID": "toolu_01Ap1",
            }),
        ),
        (
            Decision::Allow {
                input: Some(serde_json::json!({"command": "pnpm test parser"})),
                always: false,
            },
            serde_json::json!({
                "behavior": "allow",
                "updatedInput": {"command": "pnpm test parser"},
                "toolUseID": "toolu_01Ap1",
            }),
        ),
    ] {
        let fake = Fake::new("approval");
        let request = RunRequest {
            permission: Some(AgentPermission::Manual),
            approvals: true,
            ..coordinator(&fake.root())
        };
        let Started { run, mut events } = launch(&fake.backend, request).await;
        let asked = until_asked(&mut events).await;
        run.answer(Answer {
            approval_id: asked.approval_id,
            decision,
        })
        .unwrap();
        rest(&mut events).await;
        assert_eq!(fake.stdin()[1]["response"]["response"], expected);
    }
}

/// RYA-222: a `control_cancel_request` withdraws a request, whose late answer then never reaches
/// the CLI; a request that suppresses "always allow" offers none; and a control request plxd
/// doesn't serve gets an error, so the CLI doesn't wait on it.
#[tokio::test]
async fn a_withdrawn_request_takes_no_answer_and_other_control_requests_get_an_error() {
    let fake = Fake::new("approval-withdrawn");
    let request = RunRequest {
        permission: Some(AgentPermission::Auto),
        approvals: true,
        ..coordinator(&fake.root())
    };
    let Started { run, mut events } = launch(&fake.backend, request).await;
    let asked = until_asked(&mut events).await;
    assert_eq!(asked.tool_name, "Edit");
    assert_eq!(asked.always_allow, Vec::<String>::new());
    assert_eq!(
        asked.blocked_path.as_deref(),
        Some("/Users/dev/repo/.claude/settings.json")
    );
    assert_eq!(asked.subagent.as_deref(), Some("a7f3c2e1"));
    assert_eq!(
        next(&mut events).await,
        Event::ApprovalWithdrawn {
            approval_id: asked.approval_id
        }
    );
    run.answer(Answer {
        approval_id: asked.approval_id,
        decision: Decision::Allow {
            input: None,
            always: false,
        },
    })
    .unwrap();
    let all = rest(&mut events).await;
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("Stopped.".into())
        }
    );
    let stdin = fake.stdin();
    assert_eq!(
        stdin.len(),
        2,
        "the prompt and the hook's error, not the answer: {stdin:?}"
    );
    assert_eq!(
        stdin[1],
        serde_json::json!({
            "type": "control_response",
            "response": {
                "subtype": "error",
                "request_id": "req-hook",
                "error": "plxd doesn't answer hook_callback control requests",
            },
        })
    );
}

/// RYA-222: a coordinator in Plan asks to leave plan mode with `ExitPlanMode`, a question for the
/// user. Once it is approved, Claude Code runs in its pre-plan mode, which a later turn's init
/// reports, and the run goes on.
#[tokio::test]
async fn an_approved_plan_lets_a_coordinator_leave_plan_mode() {
    let fake = Fake::new("exit-plan");
    let request = RunRequest {
        permission: Some(AgentPermission::Plan),
        approvals: true,
        ..coordinator(&fake.root())
    };
    let Started { run, mut events } = launch(&fake.backend, request).await;
    let asked = until_asked(&mut events).await;
    assert_eq!(asked.tool_name, "ExitPlanMode");
    assert!(asked.interactive);
    assert_eq!(
        asked.input["plan"],
        "1. Add a README.\n2. Link it from the docs."
    );
    run.answer(Answer {
        approval_id: asked.approval_id,
        decision: Decision::Allow {
            input: None,
            always: false,
        },
    })
    .unwrap();
    run.send(FollowUp {
        turn_id: turn(TURN_2),
        text: "Go ahead.".into(),
        images: Vec::new(),
    })
    .unwrap();
    let all = rest(&mut events).await;
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("Done.".into())
        }
    );
    assert_eq!(
        fake.stdin()[1]["response"]["response"],
        serde_json::json!({
            "behavior": "allow",
            "updatedInput": {"plan": "1. Add a README.\n2. Link it from the docs."},
            "toolUseID": "toolu_01Pl1",
        })
    );
}

/// Events until the run's first permission request, which it returns.
async fn until_asked(events: &mut EventStream) -> ApprovalRequest {
    loop {
        match next(events).await {
            Event::ApprovalRequested(request) => return request,
            Event::Finished { outcome, .. } => panic!("the run ended first: {outcome:?}"),
            _ => {}
        }
    }
}

/// An init line of a coordinator in `mode`.
fn coordinator_init(mode: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"system","subtype":"init","session_id":"s","apiKeySource":"none","permissionMode":"{mode}","tools":["Read","Bash"]}}"#
    )
    .into_bytes()
}

fn asks(steps: &[Step]) -> Vec<(&ApprovalRequest, &Ask)> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Ask(request, ask) => Some((request, ask)),
            _ => None,
        })
        .collect()
}

/// RYA-222: control requests are the driver's only when the CLI was started with the prompt
/// channel; otherwise they're skipped, as before. One before the init is refused, since the CLI
/// may be on credentials nobody checked.
#[test]
fn control_requests_are_answered_only_with_the_prompt_channel() {
    let request = br#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"ls"}}}"#;
    let cancel = br#"{"type":"control_cancel_request","request_id":"r1"}"#;
    let mut without = Translator::new(ToolPolicy::NoWrite, "none")
        .with_coordinator_tools(true)
        .with_permission_mode("default");
    without.line(&coordinator_init("default"));
    assert_eq!(without.line(request), []);
    assert_eq!(without.line(cancel), []);

    let mut early = Translator::new(ToolPolicy::NoWrite, "none")
        .with_coordinator_tools(true)
        .with_permission_mode("default")
        .with_prompts(true);
    assert_eq!(
        violation_kind(&early.line(request)),
        Some(FailureKind::UnexpectedApiKey)
    );

    let mut translator = Translator::new(ToolPolicy::NoWrite, "none")
        .with_coordinator_tools(true)
        .with_permission_mode("default")
        .with_prompts(true);
    assert_eq!(
        violation_kind(&translator.line(&coordinator_init("default"))),
        None
    );
    let steps = translator.line(request);
    let [(asked, ask)] = asks(&steps)[..] else {
        panic!("{steps:?}")
    };
    assert_eq!(asked.tool_name, "Bash");
    assert_eq!(asked.input, serde_json::json!({"command": "ls"}));
    assert!(asked.always_allow.is_empty());
    assert_eq!(ask.request_id, "r1");
    assert_eq!(ask.tool_use_id, None);
    assert_eq!(translator.line(cancel), [Step::Withdraw("r1".into())]);
    let elicit = br#"{"type":"control_request","request_id":"r2","request":{"subtype":"elicitation","mcp_server_name":"x"}}"#;
    assert_eq!(
        translator.line(elicit),
        [Step::Refuse {
            request_id: "r2".into(),
            error: "plxd doesn't answer elicitation control requests".into(),
        }]
    );
    let nameless = br#"{"type":"control_request","request_id":"r3","request":{"subtype":"can_use_tool","input":{}}}"#;
    assert!(matches!(
        translator.line(nameless).as_slice(),
        [Step::Refuse { request_id, .. }] if request_id == "r3"
    ));
}

/// RYA-222: approving `ExitPlanMode` takes Claude Code to its pre-plan mode, so later inits may
/// report another mode than the one the run asked for; until then they may not.
#[test]
fn a_plan_coordinator_may_report_another_mode_only_once_it_left_plan_mode() {
    let plan = || {
        Translator::new(ToolPolicy::NoWrite, "none")
            .with_coordinator_tools(true)
            .with_permission_mode("plan")
            .with_prompts(true)
    };
    let mut translator = plan();
    assert_eq!(
        violation_kind(&translator.line(&coordinator_init("plan"))),
        None
    );
    assert_eq!(
        violation_kind(&translator.line(&coordinator_init("default"))),
        Some(FailureKind::PolicyViolation)
    );
    let mut translator = plan();
    assert_eq!(
        violation_kind(&translator.line(&coordinator_init("plan"))),
        None
    );
    translator.left_plan_mode();
    assert_eq!(
        violation_kind(&translator.line(&coordinator_init("default"))),
        None
    );
    // Only Claude Code's own modes after plan mode: never one that asks less, or none at all.
    for (mode, expected) in [
        ("acceptEdits", None),
        ("plan", None),
        ("bypassPermissions", Some(FailureKind::PolicyViolation)),
        ("auto", Some(FailureKind::PolicyViolation)),
    ] {
        let mut translator = plan();
        translator.line(&coordinator_init("plan"));
        translator.left_plan_mode();
        assert_eq!(
            violation_kind(&translator.line(&coordinator_init(mode))),
            expected,
            "{mode}"
        );
    }
    let mut translator = plan();
    translator.line(&coordinator_init("plan"));
    translator.left_plan_mode();
    let modeless = br#"{"type":"system","subtype":"init","session_id":"s","apiKeySource":"none","tools":["Read","Bash"]}"#;
    assert_eq!(
        violation_kind(&translator.line(modeless)),
        Some(FailureKind::PolicyViolation)
    );
}

/// An init line of a worker on Claude Code 2.1.283 in `mode`, listing `tools`.
fn worker_init(mode: &str, tools: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"system","subtype":"init","session_id":"s","apiKeySource":"none","claude_code_version":"2.1.283","permissionMode":"{mode}","tools":{tools}}}"#
    )
    .into_bytes()
}

/// RYA-243: a worker's init may list `ExitPlanMode` only when its `--tools` named it, and still
/// nothing else beyond the worker's tools. After the plan's approval, Claude Code still lists it
/// and reports Manual, which the check then accepts, but never a mode that asks less.
#[test]
fn a_worker_s_init_may_list_exit_plan_mode_only_when_it_hands_over_plans() {
    // What 2.1.283 lists for a plan worker that asks plxd.
    let planning = r#"["Bash","Edit","ExitPlanMode","Glob","Grep","NotebookEdit","Read","WebFetch","WebSearch","Write"]"#;
    let worker = |mode: &'static str, plan_exit: bool| {
        Translator::new(ToolPolicy::WorkspaceWrite, "none")
            .with_permission_mode(mode)
            .with_prompts(true)
            .with_plan_exit(plan_exit)
    };
    let mut translator = worker("plan", true);
    assert_eq!(
        violation_kind(&translator.line(&worker_init("plan", planning))),
        None
    );
    for mode in ["plan", "default", "auto", "acceptEdits"] {
        let mut translator = worker(mode, false);
        assert_eq!(
            violation_kind(&translator.line(&worker_init(mode, planning))),
            Some(FailureKind::PolicyViolation),
            "{mode}"
        );
    }
    for extra in [
        "AskUserQuestion",
        "EnterPlanMode",
        "Agent",
        "mcp__plxd__spawn_agent",
    ] {
        let tools = format!(r#"["Bash","ExitPlanMode","{extra}"]"#);
        let mut translator = worker("plan", true);
        assert_eq!(
            violation_kind(&translator.line(&worker_init("plan", &tools))),
            Some(FailureKind::PolicyViolation),
            "{extra}"
        );
    }
    for (mode, expected) in [
        ("default", None),
        ("acceptEdits", None),
        ("bypassPermissions", Some(FailureKind::PolicyViolation)),
        ("auto", Some(FailureKind::PolicyViolation)),
    ] {
        let mut translator = worker("plan", true);
        translator.line(&worker_init("plan", planning));
        translator.left_plan_mode();
        assert_eq!(
            violation_kind(&translator.line(&worker_init(mode, planning))),
            expected,
            "{mode}"
        );
    }
}

/// A worker in Plan at `cwd` whose client answers, so it hands its plan over (RYA-243).
fn plan_worker(cwd: &Path) -> RunRequest {
    RunRequest {
        policy: ToolPolicy::WorkspaceWrite,
        sandbox: Some(worker_sandbox(cwd)),
        permission: Some(AgentPermission::Plan),
        approvals: true,
        ..request(cwd)
    }
}

/// RYA-243: a plan worker hands its plan over with `ExitPlanMode`, as a coordinator does (0031).
/// The request is a question for the user that holds the plan, and plxd's answer goes back on
/// stdin. Once allowed, a later turn's init may report Manual. Once denied, the worker still
/// plans, so an init that reports Manual fails it.
#[tokio::test]
async fn a_plan_worker_hands_its_plan_over_and_leaves_plan_mode_only_once_allowed() {
    let plan = serde_json::json!({
        "plan": "# Plan\n1. Add a README.\n",
        "planFilePath": "/Users/dev/.claude/plans/plan-it-cozy-pinwheel.md",
    });
    let keep_planning = "Keep planning: add tests.";
    for (decision, response) in [
        (
            Decision::Allow {
                input: None,
                always: false,
            },
            serde_json::json!({"behavior": "allow", "updatedInput": plan, "toolUseID": "toolu_01Pw1"}),
        ),
        (
            Decision::Deny {
                message: keep_planning.into(),
                interrupt: false,
            },
            serde_json::json!({
                "behavior": "deny",
                "message": keep_planning,
                "interrupt": false,
                "toolUseID": "toolu_01Pw1",
            }),
        ),
    ] {
        let allowed = matches!(decision, Decision::Allow { .. });
        let fake = Fake::new("worker-exit-plan");
        let request = plan_worker(&fake.root());
        let expected: Vec<String> = super::arguments(&request)
            .unwrap()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect();
        assert!(expected.iter().any(|arg| arg == PLAN_WORKER_TOOL_LIST));
        let Started { run, mut events } = launch(&fake.backend, request).await;
        let asked = until_asked(&mut events).await;
        assert_eq!(asked.tool_name, "ExitPlanMode");
        assert!(asked.interactive);
        assert_eq!(asked.input, plan);
        assert_eq!(asked.call_id.as_deref(), Some("toolu_01Pw1"));
        assert!(asked.always_allow.is_empty());
        run.answer(Answer {
            approval_id: asked.approval_id,
            decision,
        })
        .unwrap();
        run.send(FollowUp {
            turn_id: turn(TURN_2),
            text: "Go ahead.".into(),
            images: Vec::new(),
        })
        .unwrap();
        let all = rest(&mut events).await;
        assert_eq!(fake.argv(), expected);
        assert_eq!(fake.stdin()[1]["response"]["response"], response);
        if allowed {
            assert_eq!(
                outcome(&all),
                &Outcome::Completed {
                    result: Some("Done.".into())
                }
            );
        } else {
            let (kind, message) = failure(&all);
            assert_eq!(kind, FailureKind::PolicyViolation);
            assert!(
                message.contains(r#"permission mode "default" in a worker run instead of "plan""#),
                "{message}"
            );
        }
    }
}

/// RYA-243: the backend lets only a worker that hands over plans list `ExitPlanMode`. The same
/// transcript stops a Plan worker without `approvals`, and a Manual worker with them, at its
/// first init, naming the tool, before anything asks.
#[tokio::test]
async fn a_worker_that_does_not_hand_over_plans_is_stopped_when_it_lists_exit_plan_mode() {
    for (permission, approvals) in [
        (AgentPermission::Plan, false),
        (AgentPermission::Manual, true),
    ] {
        let fake = Fake::new("worker-exit-plan");
        let request = RunRequest {
            permission: Some(permission),
            approvals,
            ..plan_worker(&fake.root())
        };
        let all = run(&fake, request).await;
        let (kind, message) = failure(&all);
        assert_eq!(kind, FailureKind::PolicyViolation, "{permission:?}");
        assert!(
            message.ends_with("in a worker run: ExitPlanMode"),
            "{permission:?}: {message}"
        );
        assert!(
            !all.iter()
                .any(|event| matches!(event, Event::ApprovalRequested(_))),
            "{all:?}"
        );
    }
}

/// RYA-222: a request offers to always allow only the rules its `approvalRequested` item shows
/// whole, at most [`MAX_ALWAYS_ALLOW_RULES`], and an answer with `always` sends exactly those.
#[test]
fn always_allow_offers_only_the_rules_the_user_is_shown() {
    let mut rules: Vec<Value> = (0..20)
        .map(|n| serde_json::json!({"toolName": "Bash", "ruleContent": format!("make {n}:*")}))
        .collect();
    let long = "x".repeat(MAX_ALWAYS_ALLOW_RULE_BYTES);
    rules.insert(
        1,
        serde_json::json!({"toolName": "Bash", "ruleContent": long}),
    );
    let request = serde_json::json!({
        "type": "control_request",
        "request_id": "r1",
        "request": {
            "subtype": "can_use_tool",
            "tool_name": "Bash",
            "input": {"command": "make 0"},
            "permission_suggestions": [{
                "type": "addRules",
                "rules": rules,
                "behavior": "allow",
                "destination": "localSettings",
            }],
        },
    });
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none")
        .with_coordinator_tools(true)
        .with_permission_mode("default")
        .with_prompts(true);
    translator.line(&coordinator_init("default"));
    let steps = translator.line(request.to_string().as_bytes());
    let [(asked, ask)] = asks(&steps)[..] else {
        panic!("{steps:?}")
    };
    assert_eq!(asked.always_allow.len(), MAX_ALWAYS_ALLOW_RULES);
    assert_eq!(asked.always_allow[0], "Bash(make 0:*)");
    assert_eq!(
        asked.always_allow[1], "Bash(make 1:*)",
        "a rule too long to show whole isn't offered"
    );
    let sent: Vec<&Value> = ask
        .updates
        .iter()
        .flat_map(|update| update["rules"].as_array().unwrap())
        .collect();
    let shown: Vec<String> = sent
        .iter()
        .map(|rule| format!("Bash({})", rule["ruleContent"].as_str().unwrap()))
        .collect();
    assert_eq!(shown, asked.always_allow, "what is sent is what is shown");
}

/// RYA-222: a request the CLI still waits on when it exits is withdrawn in the run's own events,
/// before its `Finished`, so it ends even when an account fallback (#119) runs another attempt in
/// its place.
#[tokio::test]
async fn a_request_left_waiting_when_the_cli_exits_is_withdrawn_before_the_run_ends() {
    let fake = Fake::new("approval-exit");
    let request = RunRequest {
        permission: Some(AgentPermission::Manual),
        approvals: true,
        ..coordinator(&fake.root())
    };
    let Started { mut events, .. } = launch(&fake.backend, request).await;
    let asked = until_asked(&mut events).await;
    let all = rest(&mut events).await;
    assert_eq!(
        all[all.len() - 2],
        Event::ApprovalWithdrawn {
            approval_id: asked.approval_id
        },
        "{all:?}"
    );
    assert!(matches!(outcome(&all), Outcome::Failed(_)), "{all:?}");
}
