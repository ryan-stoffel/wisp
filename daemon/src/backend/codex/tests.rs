//! The Codex backend against a fake `codex` on `PATH` that replays fixtures, most of them captured
//! from real `codex exec --json` runs, so every test spawns a real process through the
//! supervisor. No test runs the real CLI. Only macOS runs Codex workers, so these tests are
//! macOS-only; the translator's own tests in `stream.rs` run everywhere.

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parallax_protocol::{AccountChoice, AccountId, Provider, Role};
use tempfile::TempDir;

use super::{
    CodexBackend, WORKER_FEATURES, arguments, worker_overrides, write_images, write_zdotdir,
};
use crate::backend::process::{Environment, Launcher};
use crate::backend::sandbox::unreadable_in_home;
use crate::backend::{
    AccountRef, AgentEffort, AgentPermission, ApiKey, Backend, Credential, Event, EventStream,
    FailureKind, ImageMediaType, ModelUsage, Outcome, PromptImage, Resume, RunId, RunRequest,
    StartError, Started, ToolPolicy, ToolStatus, Usage, WarningKind, WorkerSandbox,
};
use crate::keystore::{KeyStore, MemoryKeyStore};
use crate::paths::DataDir;
use crate::routing::{self, BackendRegistry, Defaults, KeyAccounts};

const FAKE_CODEX: &str = include_str!("fixtures/fake-codex.sh");
const THREAD: &str = "01a0eaf8-7c27-7762-a05e-9eae0197d57c";
const TURN: &str = "01997e2a-4c3b-7d10-8a2e-5f6b7c8d9e01";
const DATA: &str = "/Users/u/Library/Application Support/parallax";
const CONTEXT: &str = "/Users/u/Library/Application Support/parallax/context/p";

fn fixture(name: &str) -> &'static str {
    match name {
        "worker" => include_str!("fixtures/worker.jsonl"),
        "resume" => include_str!("fixtures/resume.jsonl"),
        "not-signed-in" => include_str!("fixtures/not-signed-in.jsonl"),
        "usage-limit" => include_str!("fixtures/usage-limit.jsonl"),
        "throttled" => include_str!("fixtures/throttled.jsonl"),
        "mcp-call" => include_str!("fixtures/mcp-call.jsonl"),
        "cancel" => include_str!("fixtures/cancel.jsonl"),
        other => panic!("no fixture {other}"),
    }
}

/// Inherited variables that could pick Codex's credentials, endpoint, or configuration. None
/// may reach the CLI.
const INHERITED_CREDENTIALS: &[(&str, &str)] = &[
    ("OPENAI_API_KEY", "parallax-test-not-a-key"),
    ("OPENAI_BASE_URL", "https://example.invalid/v1"),
    ("CODEX_API_KEY", "parallax-test-not-a-key"),
    ("CODEX_HOME", "/tmp/parallax-test-inherited-codex-home"),
];

/// A fake `codex` on the launcher's `PATH`, in a folder that also holds what it records.
struct Fake {
    dir: TempDir,
    backend: CodexBackend,
}

impl Fake {
    fn new(fixture_name: &str) -> Self {
        Self::with_key_fixture(fixture_name, None)
    }

    /// A fake that replays `key_fixture` instead when it runs with an API key.
    fn with_key_fixture(fixture_name: &str, key_fixture: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let program = bin.join("codex");
        fs::write(&program, FAKE_CODEX).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let mut base: Environment = [
            ("PATH", format!("{}:/usr/bin:/bin", bin.display())),
            ("FAKE_CODEX_DIR", root.display().to_string()),
            ("KEPT", "yes".into()),
        ]
        .into_iter()
        .chain(
            INHERITED_CREDENTIALS
                .iter()
                .map(|(name, value)| (*name, (*value).to_owned())),
        )
        .collect();
        for (variable, name) in [
            ("FAKE_CODEX_FIXTURE", Some(fixture_name)),
            ("FAKE_CODEX_KEY_FIXTURE", key_fixture),
        ] {
            if let Some(name) = name {
                let path = root.join(format!("{name}.jsonl"));
                fs::write(&path, fixture(name)).unwrap();
                base.set(variable, path);
            }
        }
        let launcher = Launcher::new(DataDir::new(root.join("data")).unwrap(), base);
        Self {
            dir,
            backend: CodexBackend::new(launcher),
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

    /// The CLI's value for each variable in `names`, or `None` where it had none.
    fn env(&self, names: &[&str]) -> Vec<Option<String>> {
        let env = self.recorded("env");
        names
            .iter()
            .map(|name| {
                env.lines()
                    .find_map(|line| line.strip_prefix(&format!("{name}=")))
                    .map(str::to_owned)
            })
            .collect()
    }
}

fn sandbox(cwd: &Path) -> WorkerSandbox {
    WorkerSandbox::for_worktree(
        Path::new("/Users/u"),
        Path::new(DATA),
        cwd,
        Path::new("/Users/u/src/app/.git"),
        Path::new(CONTEXT),
        Path::new("/tmp/parallax-1a2b3c4d/Ab12Cd"),
    )
}

fn request(cwd: &Path) -> RunRequest {
    RunRequest {
        run_id: RunId::generate(),
        turn_id: Some(TURN.parse().unwrap()),
        cwd: cwd.to_owned(),
        prompt: "Run `echo hello`, then create hello.txt.".into(),
        images: Vec::new(),
        policy: ToolPolicy::WorkspaceWrite,
        sandbox: Some(sandbox(cwd)),
        account: AccountRef {
            id: "codex".into(),
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

async fn run(backend: &dyn Backend, request: RunRequest) -> Vec<Event> {
    rest(&mut backend.start(request).unwrap().events).await
}

fn outcome(events: &[Event]) -> &Outcome {
    match events.last() {
        Some(Event::Finished { outcome, .. }) => outcome,
        other => panic!("expected Finished last, got {other:?}"),
    }
}

fn failure(events: &[Event]) -> FailureKind {
    match outcome(events) {
        Outcome::Failed(failure) => failure.failure,
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn tokens(input: u64, output: u64, read: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: read,
        cache_write_tokens: 0,
        cost_usd_micros: None,
    }
}

/// `argv`'s `-c` values, in order.
fn overrides(argv: &[String]) -> Vec<&str> {
    argv.windows(2)
        .filter(|pair| pair[0] == "-c")
        .map(|pair| pair[1].as_str())
        .collect()
}

#[tokio::test]
async fn a_worker_run_maps_the_real_stream_and_holds_codex_to_0013() {
    let fake = Fake::new("worker");
    let cwd = fake.root();
    let mut worker = request(&cwd);
    worker.model = Some("gpt-5.5-codex".into());
    worker.effort = Some(AgentEffort::Xhigh);
    worker.account.credential = Credential::Subscription {
        config_home: Some("/tmp/codex-second-account".into()),
    };
    let events = run(&fake.backend, worker).await;

    let call = |item: &str| format!("{TURN}-{item}");
    let path = "/Users/u/Library/Application Support/parallax/worktrees/app-1a2b/run/hello.txt";
    let session = Event::SessionStarted {
        session_id: THREAD.into(),
        model: None,
        api_key_source: None,
    };
    let text = |text: &str| Event::Text {
        message_id: None,
        text: text.into(),
    };
    assert_eq!(
        events,
        [
            Event::TurnStarted {
                turn_id: Some(TURN.parse().unwrap())
            },
            session,
            text(
                "1. Run `echo hello` in the shell.\n2. Create `hello.txt` containing `hi` using \
                 `apply_patch`.\n\nA plan tool isn’t available in this session, so I’ve stated \
                 the plan here.\n"
            ),
            Event::ToolCall {
                call_id: call("item_1"),
                name: "command_execution".into(),
                input: serde_json::json!({"command": "/bin/zsh -lc 'echo hello'"}),
            },
            Event::ToolResult {
                call_id: call("item_1"),
                status: ToolStatus::Ok,
                output: Some("hello\n".into()),
            },
            Event::ToolCall {
                call_id: call("item_2"),
                name: "file_change".into(),
                input: serde_json::json!({"changes": [{"path": path, "kind": "add"}]}),
            },
            Event::ToolResult {
                call_id: call("item_2"),
                status: ToolStatus::Ok,
                output: None,
            },
            text("done"),
            // input_tokens counts cached input too: 55274 - 36352.
            Event::Usage(ModelUsage {
                model: None,
                usage: tokens(18_922, 184, 36_352),
            }),
            Event::TurnFinished {
                turn_id: Some(TURN.parse().unwrap()),
                result: Some("done".into()),
            },
            Event::Finished {
                outcome: Outcome::Completed {
                    result: Some("done".into())
                },
                usage_totals: vec![ModelUsage {
                    model: None,
                    usage: tokens(18_922, 184, 36_352),
                }],
            },
        ]
    );

    assert_worker_invocation(&fake);
}

/// A worker's prompt, sandbox, `ZDOTDIR`, model, and second account reached the CLI, and nothing
/// else did.
fn assert_worker_invocation(fake: &Fake) {
    let cwd = fake.root();
    // The prompt went on stdin, never in argv, and stdin closed after it.
    assert_eq!(
        fake.recorded("stdin"),
        "Run `echo hello`, then create hello.txt."
    );
    let argv = fake.argv();
    assert_eq!(
        argv[..4],
        ["exec", "--json", "--ignore-user-config", "--ignore-rules"]
    );
    assert_eq!(argv[argv.len() - 3..], ["-m", "gpt-5.5-codex", "-"]);
    let cwd_text = cwd.display().to_string();
    let values = overrides(&argv);
    assert_eq!(values.len(), 9, "{values:?}");
    // The worker's ZDOTDIR, in the data folder's tmp/, is gone once the run has finished.
    let zdotdir = values[7]
        .strip_prefix(r#"shell_environment_policy={ignore_default_excludes=false, set={ZDOTDIR=""#)
        .and_then(|rest| rest.strip_suffix(r#""}}"#))
        .unwrap_or_else(|| panic!("{}", values[7]));
    assert!(
        zdotdir.starts_with(&format!("{cwd_text}/data/tmp/codex-zdotdir-")),
        "{zdotdir}"
    );
    assert!(!Path::new(zdotdir).exists(), "{zdotdir} outlived the run");
    assert_eq!(values[0], r#"default_permissions="parallax_worker""#);
    let permissions = values[1]
        .strip_prefix(&format!(
            r#"permissions={{parallax_worker={{extends=":workspace", workspace_roots={{"{CONTEXT}"=true}}, filesystem={{"#
        ))
        .and_then(|rest| {
            rest.strip_suffix(r#"}, network={enabled=true, domains={"*"="allow"}}}}"#)
        })
        .unwrap_or_else(|| panic!("{}", values[1]));
    let rules: BTreeSet<&str> = permissions.split(", ").collect();
    let mut expected: BTreeSet<String> = unreadable_in_home()
        .map(|path| format!(r#""/Users/u/{path}"="deny""#))
        .collect();
    let uid = rustix::process::getuid().as_raw();
    expected.extend([
        format!(r#""{DATA}"="deny""#),
        r#""/tmp/parallax-1a2b3c4d"="deny""#.to_owned(),
        format!(r#""/private/tmp/claude-{uid}"="deny""#),
        r#""/tmp/codex-second-account"="deny""#.to_owned(),
        format!(r#""{cwd_text}/.git"="read""#),
        r#""/Users/u/src/app/.git"="read""#.to_owned(),
        format!(r#""{zdotdir}"="read""#),
    ]);
    assert_eq!(rules, expected.iter().map(String::as_str).collect());
    assert_eq!(
        values[2..],
        [
            "features={network_proxy=true, hooks=false, apps=false, plugins=false, \
             remote_plugin=false, multi_agent=false, skill_mcp_dependency_install=false, \
             shell_snapshot=false}",
            &format!(r#"projects={{"{cwd_text}"={{trust_level="untrusted"}}}}"#),
            r#"approval_policy="never""#,
            r#"web_search="live""#,
            "allow_login_shell=false",
            &format!(
                r#"shell_environment_policy={{ignore_default_excludes=false, set={{ZDOTDIR="{zdotdir}"}}}}"#
            ),
            r#"model_reasoning_effort="xhigh""#,
        ]
    );
    assert_eq!(values[2], WORKER_FEATURES);
    for flag in [
        "-s",
        "--sandbox",
        "--dangerously-bypass-approvals-and-sandbox",
        "--add-dir",
    ] {
        assert!(!argv.iter().any(|arg| arg == flag), "{flag}: {argv:?}");
    }

    // Only the account's own configuration folder reaches the CLI.
    assert_eq!(
        fake.env(&[
            "OPENAI_API_KEY",
            "OPENAI_BASE_URL",
            "CODEX_API_KEY",
            "CODEX_HOME",
            "KEPT",
            "PWD"
        ]),
        [
            None,
            None,
            None,
            Some("/tmp/codex-second-account".into()),
            Some("yes".into()),
            Some(cwd_text)
        ]
    );
}

#[tokio::test]
async fn a_resumed_thread_reports_only_what_it_adds() {
    let fake = Fake::new("resume");
    let mut resumed = request(&fake.root());
    resumed.resume = Some(Resume {
        session_id: THREAD.into(),
        usage_totals: vec![ModelUsage {
            model: None,
            usage: tokens(18_922, 184, 36_352),
        }],
    });
    let events = run(&fake.backend, resumed).await;

    let argv = fake.argv();
    assert_eq!(argv[..2], ["exec", "resume"]);
    assert_eq!(argv[argv.len() - 2..], [THREAD, "-"]);
    // The real resume's running total: input 73838 with 54656 cached, output 190.
    let usage: Vec<&Usage> = events
        .iter()
        .filter_map(|event| match event {
            Event::Usage(delta) => Some(&delta.usage),
            _ => None,
        })
        .collect();
    assert_eq!(usage, [&tokens(260, 6, 18_304)]);
    assert_eq!(
        outcome(&events),
        &Outcome::Completed {
            result: Some("hello.txt".into())
        }
    );
}

/// A resumed thread's images reach `codex exec resume` as `--image=` files in the data folder's
/// `tmp/`, which are gone once it exits (RYA-191).
#[tokio::test]
async fn images_reach_codex_as_files_that_go_when_it_exits() {
    let fake = Fake::new("resume");
    let mut resumed = request(&fake.root());
    resumed.resume = Some(Resume::new(THREAD));
    resumed.images = vec![PromptImage {
        media_type: ImageMediaType::Png,
        data: "iVBORw0KGgo=".into(),
    }];
    run(&fake.backend, resumed).await;

    let argv = fake.argv();
    let [.., image, thread, dash] = &argv[..] else {
        panic!("{argv:?}");
    };
    assert_eq!([thread.as_str(), dash.as_str()], [THREAD, "-"]);
    let path = Path::new(image.strip_prefix("--image=").unwrap());
    assert!(path.starts_with(fake.root().join("data/tmp")), "{image}");
    assert_eq!(path.extension().unwrap(), "png");
    assert!(!path.parent().unwrap().exists(), "the images go with codex");
    assert!(
        !fake.recorded("stdin").contains("png"),
        "the prompt never names them"
    );
}

#[test]
fn image_files_hold_the_decoded_bytes_in_a_private_folder() {
    let dir = tempfile::tempdir().unwrap();
    assert!(write_images(dir.path(), &[]).unwrap().is_none());
    let jpeg = PromptImage {
        media_type: ImageMediaType::Jpeg,
        data: "/9j/".into(),
    };
    let (folder, paths) = write_images(dir.path(), &[jpeg]).unwrap().unwrap();
    assert_eq!(paths, [folder.path().join("1.jpg")]);
    assert_eq!(fs::read(&paths[0]).unwrap(), b"\xff\xd8\xff");
    let mode = fs::metadata(folder.path()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
}

#[tokio::test]
async fn an_api_key_account_gets_only_its_key() {
    let fake = Fake::new("worker");
    let mut keyed = request(&fake.root());
    keyed.account.credential = Credential::ApiKey(ApiKey::new("sk-proj-parallax-test".into()));
    let events = run(&fake.backend, keyed).await;
    assert!(matches!(outcome(&events), Outcome::Completed { .. }));
    assert_eq!(
        fake.env(&["CODEX_API_KEY", "OPENAI_API_KEY", "CODEX_HOME"]),
        [Some("sk-proj-parallax-test".into()), None, None]
    );
    assert!(!fake.argv().iter().any(|arg| arg.contains("sk-proj")));
}

/// Routing's key accounts: one `OpenAI` key to fall back to.
struct OneKey(AccountId);

impl KeyAccounts for OneKey {
    fn provider_of(&self, id: AccountId) -> Option<Provider> {
        (id == self.0).then_some(Provider::Openai)
    }

    fn fallback_for(&self, provider: Provider) -> Option<AccountId> {
        (provider == Provider::Openai).then_some(self.0)
    }
}

#[tokio::test]
async fn a_signed_out_or_limited_login_falls_back_to_an_openai_key() {
    for (first, reason) in [
        ("not-signed-in", FailureKind::NotSignedIn),
        ("usage-limit", FailureKind::RateLimited),
        ("throttled", FailureKind::RateLimited),
    ] {
        let fake = Fake::with_key_fixture(first, Some("worker"));
        let mut backends = BackendRegistry::new();
        backends.register(Provider::Openai, Arc::new(fake.backend.clone()));
        let key = AccountId::generate();
        let store = MemoryKeyStore::new();
        store.set(key, "sk-proj-fallback").unwrap();
        let resolved = routing::resolve(
            &backends,
            &OneKey(key),
            &Defaults::default(),
            Role::Worker,
            Some(AccountChoice::Subscription {
                backend: "codex".into(),
            }),
            ToolPolicy::WorkspaceWrite,
        )
        .unwrap();
        let mut started = routing::start(
            Arc::new(store),
            &OneKey(key),
            resolved,
            request(&fake.root()),
        )
        .unwrap();
        let events = rest(&mut started.events).await;
        assert!(
            events.contains(&Event::AccountFallback {
                from_account: "codex".into(),
                to_account: key.to_string(),
                reason,
            }),
            "{first}: {events:?}"
        );
        assert!(
            matches!(outcome(&events), Outcome::Completed { .. }),
            "{first}"
        );
        assert_eq!(
            fake.env(&["CODEX_API_KEY"]),
            [Some("sk-proj-fallback".into())]
        );
    }
}

#[tokio::test]
async fn an_mcp_call_stops_the_worker_at_once() {
    let fake = Fake::new("mcp-call");
    let started = Instant::now();
    let events = run(&fake.backend, request(&fake.root())).await;
    assert_eq!(failure(&events), FailureKind::PolicyViolation);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn cancel_interrupts_codex_with_sigint() {
    let fake = Fake::new("cancel");
    let Started { run, mut events } = fake.backend.start(request(&fake.root())).unwrap();
    assert!(matches!(next(&mut events).await, Event::TurnStarted { .. }));
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    // `@trap-armed`, printed once the fake's SIGINT trap is installed: a deterministic handshake.
    // The fake then hangs in foreground sleeps, so a trap bash left pending still runs (RYA-120).
    assert!(matches!(
        next(&mut events).await,
        Event::Warning {
            warning: WarningKind::MalformedLine,
            ..
        }
    ));
    assert!(matches!(next(&mut events).await, Event::Text { .. }));
    assert!(matches!(next(&mut events).await, Event::ToolCall { .. }));
    let started = Instant::now();
    run.cancel();
    assert_eq!(outcome(&rest(&mut events).await), &Outcome::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(fake.recorded("signals"), "SIGINT\n");
}

#[tokio::test]
async fn requests_codex_can_t_run_are_refused_before_spawning() {
    let fake = Fake::new("worker");
    let cwd = fake.root();
    let refuse = |request: RunRequest| fake.backend.start(request).map(|_| ()).unwrap_err();

    let mut no_write = request(&cwd);
    no_write.policy = ToolPolicy::NoWrite;
    assert!(matches!(refuse(no_write), StartError::Unsupported(_)));
    let mut no_sandbox = request(&cwd);
    no_sandbox.sandbox = None;
    assert!(matches!(refuse(no_sandbox), StartError::Invalid(_)));
    let mut option = request(&cwd);
    option.model = Some("--dangerously-bypass-approvals-and-sandbox".into());
    assert!(matches!(refuse(option), StartError::Invalid(_)));
    let mut plan = request(&cwd);
    plan.permission = Some(AgentPermission::Plan);
    assert!(matches!(refuse(plan), StartError::Unsupported(_)));
    let mut window = request(&cwd);
    window.context_window = Some(1_000_000);
    assert!(matches!(refuse(window), StartError::Unsupported(_)));
    assert!(fake.argv().is_empty(), "nothing was spawned");
    let comma = [PathBuf::from("/Users/a,b/1.png")];
    assert!(matches!(
        arguments(&request(&cwd), None, &comma),
        Err(StartError::Invalid(_))
    ));
}

/// A context window and fast mode are `-c` overrides on a new and a resumed session alike: fast
/// is the `priority` service tier, and standard the `default` one.
#[test]
fn a_context_window_and_fast_mode_are_overrides_on_every_session() {
    let fake = Fake::new("worker");
    let mut worker = request(&fake.root());
    worker.context_window = Some(872_000);
    for (fast, tier) in [(true, "priority"), (false, "default")] {
        worker.fast = Some(fast);
        for resume in [None, Some("thread-1")] {
            worker.resume = resume.map(|id| Resume {
                session_id: id.into(),
                usage_totals: Vec::new(),
            });
            let argv: Vec<String> = arguments(&worker, None, &[])
                .unwrap()
                .into_iter()
                .map(|arg| arg.into_string().unwrap())
                .collect();
            let values = overrides(&argv);
            let tier = format!(r#"service_tier="{tier}""#);
            assert_eq!(
                values[values.len() - 2..],
                ["model_context_window=872000", &tier]
            );
        }
    }
}

#[test]
fn paths_are_quoted_as_toml_strings() {
    let cwd = Path::new("/Users/u/we\"ird\\dir\u{7f}");
    let values = worker_overrides(&sandbox(cwd), cwd, None, None);
    assert_eq!(
        values[3],
        r#"projects={"/Users/u/we\"ird\\dir\u007F"={trust_level="untrusted"}}"#
    );
}

/// A worker's `.zshenv`, read by a real zsh whose `~/.zshenv` sets `PATH` outright as
/// nix-darwin's `/etc/zshenv` does, puts plxd's `PATH` back in front of it and leaves `ZDOTDIR`
/// unset. Only its owner may open the folder, which goes when dropped (RYA-141).
#[test]
fn a_worker_s_zdotdir_puts_its_path_back_after_zsh_startup() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let home = root.join("home");
    let tools = root.join("it's tools");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&tools).unwrap();
    fs::write(home.join(".zshenv"), "export PATH=/usr/bin:/bin\n").unwrap();
    let tool = tools.join("parallax-path-probe");
    fs::write(&tool, "#!/bin/sh\necho found-the-tool\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:/usr/bin:/bin", tools.display());

    let zdotdir = write_zdotdir(&root.join("data/tmp"), path.as_ref()).unwrap();
    for (entry, expected) in [
        (zdotdir.path().to_owned(), 0o700),
        (zdotdir.path().join(".zshenv"), 0o600),
    ] {
        let mode = fs::metadata(&entry).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, expected, "{}", entry.display());
    }
    let output = std::process::Command::new("/bin/zsh")
        .args([
            "-c",
            r#"parallax-path-probe; printf '%s\n' "$PATH" "${ZDOTDIR-unset}""#,
        ])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", &path)
        .env("ZDOTDIR", zdotdir.path())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("found-the-tool\n{path}:/usr/bin:/bin\nunset\n"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let folder = zdotdir.path().to_owned();
    drop(zdotdir);
    assert!(!folder.exists());
}
