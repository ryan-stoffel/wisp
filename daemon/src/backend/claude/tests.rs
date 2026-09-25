//! The Claude backend against a fake `claude` on `PATH` that replays synthetic fixtures, so every
//! test spawns a real process through the supervisor. No test runs the real CLI.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;

use super::stream::{Step, Translator};
use super::{ClaudeBackend, NO_WRITE_ARGS};
use crate::backend::process::{CancelPolicy, Environment, Launcher, SpawnError};
use crate::backend::{
    AccountRef, ApiKey, Backend, Credential, Event, EventStream, FailureKind, FollowUp,
    LimitStatus, LimitWindow, ModelUsage, Outcome, Resume, RunId, RunRequest, SendError,
    StartError, Started, TodoItem, TodoStatus, ToolPolicy, ToolStatus, TurnId, Usage, WarningKind,
};
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
        "write-tools" => include_str!("fixtures/write-tools.jsonl"),
        "malformed" => include_str!("fixtures/malformed.jsonl"),
        "follow-up-folded" => include_str!("fixtures/follow-up-folded.jsonl"),
        "follow-up-turns" => include_str!("fixtures/follow-up-turns.jsonl"),
        "cancel" => include_str!("fixtures/cancel.jsonl"),
        "stubborn" => include_str!("fixtures/stubborn.jsonl"),
        other => panic!("no fixture {other}"),
    }
}

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
            ("ANTHROPIC_API_KEY", "wisp-test-not-a-key".into()),
            ("ANTHROPIC_AUTH_TOKEN", "wisp-test-not-a-bearer".into()),
            ("CLAUDE_CODE_OAUTH_TOKEN", "wisp-test-not-a-token".into()),
            ("SSH_CONNECTION", "10.0.0.2 50000 10.0.0.1 22".into()),
            ("KEPT", "yes".into()),
        ]
        .into_iter()
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
        policy: ToolPolicy::NoWrite,
        account: AccountRef {
            id: "claude-max".into(),
            credential: Credential::Subscription { config_home: None },
        },
        resume: None,
        model: None,
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
                text: "The README describes wisp, a macOS editor with a host daemon.".into(),
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
                    "The README describes wisp, a macOS editor with a host daemon.".into()
                ),
            },
            Event::Finished {
                outcome: Outcome::Completed {
                    result: Some(
                        "The README describes wisp, a macOS editor with a host daemon.".into()
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
    expected.extend(NO_WRITE_ARGS);
    assert_eq!(fake.argv(), expected);
    assert_eq!(
        NO_WRITE_ARGS.join(" "),
        r#"--tools Read,Glob,Grep --setting-sources user --settings {"disableAllHooks":true} --strict-mcp-config --permission-mode dontAsk"#,
        "0004's no-write policy, exactly"
    );
    let env = fake.env();
    let working_dir = format!("PWD={}", cwd.display());
    assert!(env.contains(&working_dir), "{env:?}");
    for scrubbed in [
        "ANTHROPIC_API_KEY=",
        "ANTHROPIC_AUTH_TOKEN=",
        "CLAUDE_CODE_OAUTH_TOKEN=",
        "SSH_CONNECTION=",
        "CLAUDE_CONFIG_DIR=",
    ] {
        assert!(
            !env.iter().any(|var| var.starts_with(scrubbed)),
            "{scrubbed} leaked: {env:?}"
        );
    }
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

/// A worker's policy, model, and second account reached the CLI.
fn assert_worker_invocation(fake: &Fake) {
    let argv = fake.argv();
    assert_eq!(
        argv[6..],
        [
            "--permission-mode",
            "acceptEdits",
            "--model",
            "claude-sonnet-4-6"
        ]
    );
    assert!(!argv.iter().any(|arg| arg == "--tools"), "{argv:?}");
    assert!(
        fake.env()
            .iter()
            .any(|var| var == "CLAUDE_CONFIG_DIR=/tmp/claude-second-account")
    );
}

#[tokio::test]
async fn a_worker_run_edits_in_its_cwd_and_reports_its_tool_calls() {
    let fake = Fake::new("tool-call");
    let mut request = request(&fake.root());
    request.policy = ToolPolicy::WorkspaceWrite;
    request.model = Some("claude-sonnet-4-6".into());
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
        ]
    );
    assert_eq!(failure(&all).0, FailureKind::RateLimited);
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
async fn malformed_and_unknown_lines_are_skipped_with_warnings() {
    let fake = Fake::new("malformed");
    let all = run(&fake, request(&fake.root())).await;
    assert_eq!(
        warnings(&all),
        [
            WarningKind::MalformedLine,
            WarningKind::MalformedLine,
            WarningKind::MalformedLine,
            WarningKind::UnknownEvent,
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
            text: "too late".into()
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
async fn cancel_interrupts_the_cli_with_sigint() {
    let fake = Fake::new("cancel");
    let Started { run, mut events } = launch(&fake.backend, request(&fake.root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
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
    let mut api_key = request(&cwd);
    api_key.account.credential = Credential::ApiKey(ApiKey::new("wisp-test-not-a-key".into()));
    assert!(matches!(
        fake.backend.start(api_key),
        Err(StartError::Unsupported(_))
    ));
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
fn output_before_the_init_is_refused() {
    let mut translator = Translator::new(ToolPolicy::NoWrite, "none");
    let assistant =
        br#"{"type":"assistant","message":{"id":"m","content":[{"type":"text","text":"hi"}]}}"#;
    assert!(matches!(
        translator.line(assistant).as_slice(),
        [Step::Violation(failure)] if failure.failure == FailureKind::UnexpectedApiKey
    ));
}
