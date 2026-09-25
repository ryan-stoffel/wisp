use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rustix::process::Pid;
use serde_json::json;

use super::{FakeBackend, Script, Step};
use crate::backend::process::{CancelPolicy, Environment, Launcher, OutputLimits};
use crate::backend::{
    AccountRef, ApiKey, Backend, Credential, Event, EventStream, FailureKind, FollowUp,
    LimitStatus, ModelUsage, Outcome, Resume, RunId, RunRequest, SendError, StartError, Started,
    ToolPolicy, ToolStatus, TurnId, Usage, WarningKind, WorkerSandbox,
};
use crate::paths::DataDir;

const DATA_DIR: &str = "/tmp/wispd-fake-data";

fn fixture(name: &str) -> Script {
    let json = match name {
        "stream" => include_str!("fixtures/stream.json"),
        "context" => include_str!("fixtures/context.json"),
        "resume" => include_str!("fixtures/resume.json"),
        "hang" => include_str!("fixtures/hang.json"),
        "stubborn" => include_str!("fixtures/stubborn.json"),
        "follow-up" => include_str!("fixtures/follow-up.json"),
        "malformed" => include_str!("fixtures/malformed.json"),
        "crash" => include_str!("fixtures/crash.json"),
        "exit-code" => include_str!("fixtures/exit-code.json"),
        "vendor-error" => include_str!("fixtures/vendor-error.json"),
        other => panic!("no fixture {other}"),
    };
    Script::from_json(json).unwrap()
}

fn launcher() -> Launcher {
    let base: Environment = [
        ("PATH", "/usr/bin:/bin"),
        ("SSH_CONNECTION", "10.0.0.2 50000 10.0.0.1 22"),
        ("FAKE_API_KEY", "leaked-from-wispd"),
    ]
    .into_iter()
    .collect();
    Launcher::new(DataDir::new(DATA_DIR).unwrap(), base)
}

fn backend(name: &str) -> FakeBackend {
    FakeBackend::new(launcher(), fixture(name))
}

fn subscription() -> AccountRef {
    AccountRef {
        id: "claude-max".into(),
        credential: Credential::Subscription { config_home: None },
    }
}

fn request(cwd: &Path) -> RunRequest {
    RunRequest {
        run_id: RunId::generate(),
        cwd: cwd.to_owned(),
        prompt: "Summarize the README.".into(),
        policy: ToolPolicy::NoWrite,
        sandbox: None,
        account: subscription(),
        turn_id: None,
        resume: None,
        model: None,
    }
}

fn root() -> PathBuf {
    PathBuf::from("/")
}

/// Starts a run and checks that it opens with the prompt's turn.
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

fn outcome(events: &[Event]) -> &Outcome {
    match events.last() {
        Some(Event::Finished { outcome, .. }) => outcome,
        other => panic!("expected Finished last, got {other:?}"),
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

#[tokio::test]
async fn start_passes_the_task_to_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().canonicalize().unwrap();
    let mut request = request(&cwd);
    request.prompt = "Say \"hi\"\nthen stop.".into();
    let Started { run, mut events } = launch(&backend("context"), request.clone()).await;
    assert_eq!(run.id(), request.run_id);
    let all = rest(&mut events).await;
    assert_eq!(
        texts(&all),
        [
            cwd.to_str().unwrap(),
            "Say \"hi\"\nthen stop.",
            "no-write",
            "<unset>",
            "<unset>",
            "<unset>",
            DATA_DIR,
        ]
    );
    assert_eq!(outcome(&all), &Outcome::Completed { result: None });
}

#[tokio::test]
async fn an_api_key_account_gets_its_key_and_a_subscription_its_config_home() {
    let mut request = request(&root());
    request.policy = ToolPolicy::WorkspaceWrite;
    assert!(
        matches!(
            backend("context").start(request.clone()),
            Err(StartError::Invalid(message)) if message.contains("0013")
        ),
        "a worker needs its sandbox"
    );
    request.sandbox = Some(WorkerSandbox::for_worktree(
        Path::new("/Users/u"),
        Path::new("/Users/u/wisp"),
        &root(),
        Path::new("/Users/u/src/app/.git"),
        Path::new("/Users/u/wisp/context/p"),
    ));
    request.account.credential = Credential::ApiKey(ApiKey::new("sk-fake-123".into()));
    let mut events = launch(&backend("context"), request.clone()).await.events;
    let all = rest(&mut events).await;
    assert_eq!(
        texts(&all)[2..5],
        ["workspace-write", "sk-fake-123", "<unset>"]
    );

    request.account.credential = Credential::Subscription {
        config_home: Some("/tmp/second-account".into()),
    };
    let mut events = launch(&backend("context"), request).await.events;
    let all = rest(&mut events).await;
    assert_eq!(texts(&all)[3..5], ["<unset>", "/tmp/second-account"]);
}

#[tokio::test]
async fn events_stream_in_order_and_usage_adds_up() {
    let mut events = launch(&backend("stream"), request(&root())).await.events;
    let all = rest(&mut events).await;
    let kinds: Vec<&str> = all
        .iter()
        .map(|event| match event {
            Event::SessionStarted { .. } => "session",
            Event::TextDelta { .. } => "delta",
            Event::Text { .. } => "text",
            Event::ToolCall { .. } => "call",
            Event::ToolResult { .. } => "result",
            Event::Usage(_) => "usage",
            Event::RateLimit(_) => "limit",
            Event::TurnFinished { .. } => "turn",
            Event::Finished { .. } => "finished",
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "session", "delta", "delta", "text", "call", "result", "call", "result", "usage",
            "limit", "usage", "text", "turn", "finished"
        ]
    );
    assert_eq!(
        all[0],
        Event::SessionStarted {
            session_id: "fake-session-1".into(),
            model: Some("fake-model".into()),
            api_key_source: None,
        }
    );
    assert_eq!(
        all[4],
        Event::ToolCall {
            call_id: "c1".into(),
            name: "Read".into(),
            input: json!({"file_path": "README.md"}),
        }
    );
    assert!(matches!(
        &all[7],
        Event::ToolResult {
            status: ToolStatus::Denied,
            output: None,
            ..
        }
    ));
    let Event::RateLimit(window) = &all[9] else {
        panic!()
    };
    assert_eq!(window.status, LimitStatus::Allowed);
    assert_eq!(
        window.resets_at,
        Some("2026-09-24T17:00:00Z".parse().unwrap())
    );
    assert_eq!(
        all[12],
        Event::TurnFinished {
            turn_id: None,
            result: Some("The README is one line.".into()),
        }
    );
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("The README is one line.".into())
        }
    );
    assert_eq!(
        events.usage(),
        Usage {
            input_tokens: 1500,
            output_tokens: 120,
            cache_read_tokens: 5000,
            cache_write_tokens: 300,
            cost_usd_micros: Some(25_000),
        }
    );
    assert_eq!(events.outcome(), Some(outcome(&all)));
}

#[tokio::test]
async fn the_stream_works_as_a_futures_stream() {
    let events = launch(&backend("resume"), request(&root())).await.events;
    let all: Vec<Event> = tokio::time::timeout(Duration::from_secs(10), events.collect())
        .await
        .unwrap();
    assert_eq!(all.len(), 3);
    assert!(all[2].is_terminal());
}

#[tokio::test]
async fn cancel_mid_stream_interrupts_the_cli() {
    let Started { run, mut events } = launch(&backend("hang"), request(&root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    let Event::Text { text: pid, .. } = next(&mut events).await else {
        panic!("no pid")
    };
    assert!(matches!(next(&mut events).await, Event::TextDelta { .. }));
    let started = Instant::now();
    run.cancel();
    run.cancel();
    let all = rest(&mut events).await;
    assert_eq!(all.len(), 1, "{all:?}");
    assert_eq!(outcome(&all), &Outcome::Cancelled);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    wait_until_gone(&pid).await;
    assert_eq!(
        run.send(FollowUp {
            turn_id: TurnId::generate(),
            text: "too late".into()
        }),
        Err(SendError::Finished)
    );
}

#[tokio::test]
async fn cancel_escalates_to_sigkill_after_the_grace_period() {
    let backend = backend("stubborn").with_cancel_policy(CancelPolicy {
        grace: Duration::from_millis(300),
        ..CancelPolicy::default()
    });
    let Started { run, mut events } = launch(&backend, request(&root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
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
async fn a_follow_up_becomes_the_next_turn() {
    let Started { run, mut events } = launch(&backend("follow-up"), request(&root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    assert!(matches!(next(&mut events).await, Event::Text { .. }));
    assert_eq!(
        next(&mut events).await,
        Event::TurnFinished {
            turn_id: None,
            result: Some("First answer.".into())
        }
    );
    let turn_id = TurnId::generate();
    let follow_up = FollowUp {
        turn_id,
        text: "Now the \"tests\",\nplease.".into(),
    };
    run.send(follow_up.clone()).unwrap();
    run.send(follow_up.clone()).unwrap();
    assert_eq!(
        run.send(FollowUp {
            text: "different".into(),
            ..follow_up
        }),
        Err(SendError::IdConflict)
    );
    let all = rest(&mut events).await;
    assert_eq!(
        all,
        [
            Event::TurnStarted {
                turn_id: Some(turn_id)
            },
            Event::Text {
                message_id: None,
                text: "Now the \"tests\",\nplease.".into()
            },
            Event::TurnFinished {
                turn_id: Some(turn_id),
                result: Some("Second answer.".into())
            },
            Event::Finished {
                outcome: Outcome::Completed {
                    result: Some("Second answer.".into())
                },
                usage_totals: Vec::new(),
            },
        ]
    );
}

#[tokio::test]
async fn a_follow_up_the_cli_never_read_is_reported_dropped() {
    let Started { run, mut events } = launch(&backend("hang"), request(&root())).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    run.cancel();
    let turn_id = TurnId::generate();
    // Accepted while the run is still winding down, or refused once it has ended.
    let sent = run.send(FollowUp {
        turn_id,
        text: "one more thing".into(),
    });
    let all = rest(&mut events).await;
    assert_eq!(outcome(&all), &Outcome::Cancelled);
    if sent.is_ok() {
        assert!(
            all.iter().any(|event| match event {
                Event::TurnStarted { turn_id: t } => *t == Some(turn_id),
                Event::FollowUpDropped { turn_id: t } => *t == turn_id,
                _ => false,
            }),
            "{all:?}"
        );
    }
    assert_eq!(
        run.send(FollowUp {
            turn_id,
            text: "one more thing".into(),
        }),
        Err(SendError::Finished),
        "a retry must not claim the dropped message arrived"
    );
}

#[tokio::test]
async fn turns_finish_in_the_order_they_started() {
    let script = Script {
        steps: vec![
            Step::Init {
                session_id: "queue-1".into(),
                model: None,
            },
            Step::SleepMs(300),
            Step::EndTurn {
                result: Some("one".into()),
            },
            Step::AwaitFollowUp,
            Step::EndTurn {
                result: Some("two".into()),
            },
            Step::AwaitFollowUp,
            Step::EndTurn {
                result: Some("three".into()),
            },
        ],
    };
    let mut request = request(&root());
    let first = TurnId::generate();
    request.turn_id = Some(first);
    let Started { run, mut events } = launch(&FakeBackend::new(launcher(), script), request).await;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { .. }
    ));
    // Both reach the CLI while the prompt's turn is still running.
    let (second, third) = (TurnId::generate(), TurnId::generate());
    for (turn_id, text) in [(second, "second"), (third, "third")] {
        run.send(FollowUp {
            turn_id,
            text: text.into(),
        })
        .unwrap();
    }
    let all = rest(&mut events).await;
    let finished: Vec<(Option<TurnId>, Option<&str>)> = all
        .iter()
        .filter_map(|event| match event {
            Event::TurnFinished { turn_id, result } => Some((*turn_id, result.as_deref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        finished,
        [
            (Some(first), Some("one")),
            (Some(second), Some("two")),
            (Some(third), Some("three")),
        ]
    );
}

#[tokio::test]
async fn cancel_works_while_the_consumer_is_not_reading() {
    let mut steps = vec![Step::EchoPid];
    steps.extend((0..600).map(|n| {
        Step::Emit(Event::TextDelta {
            message_id: None,
            text: format!("chunk {n}"),
        })
    }));
    steps.push(Step::Hang);
    let backend = FakeBackend::new(launcher(), Script { steps }).with_cancel_policy(CancelPolicy {
        grace: Duration::from_millis(200),
        ..CancelPolicy::default()
    });
    let Started { run, mut events } = launch(&backend, request(&root())).await;
    let Event::Text { text: pid, .. } = next(&mut events).await else {
        panic!("no pid")
    };
    // Let the event buffer fill, so the driver is stuck waiting for this consumer.
    tokio::time::sleep(Duration::from_millis(300)).await;
    run.cancel();
    wait_until_gone(&pid).await;
    let all = rest(&mut events).await;
    assert_eq!(outcome(&all), &Outcome::Cancelled);
}

#[tokio::test]
async fn a_resumed_session_reports_only_its_own_usage() {
    let opus = |input| ModelUsage {
        model: Some("opus".into()),
        usage: Usage {
            input_tokens: input,
            output_tokens: input / 10,
            ..Usage::default()
        },
    };
    let script = Script {
        steps: vec![
            Step::Init {
                session_id: "fresh-usage".into(),
                model: None,
            },
            Step::UsageTotal(opus(1200)),
            Step::UsageTotal(opus(1250)),
            Step::EndTurn { result: None },
        ],
    };
    let backend = FakeBackend::new(launcher(), script);

    let mut fresh = launch(&backend, request(&root())).await.events;
    let all = rest(&mut fresh).await;
    let deltas = |all: &[Event]| -> Vec<u64> {
        all.iter()
            .filter_map(|event| match event {
                Event::Usage(delta) => Some(delta.usage.input_tokens),
                _ => None,
            })
            .collect()
    };
    assert_eq!(deltas(&all), [1200, 50]);
    assert_eq!(fresh.usage_totals(), [opus(1250)]);

    // The vendor's totals carry over into the resumed session; the run counts what it added.
    let mut resumed = request(&root());
    resumed.resume = Some(Resume {
        session_id: "fresh-usage".into(),
        usage_totals: vec![opus(1000)],
    });
    let mut events = launch(&backend, resumed).await.events;
    let all = rest(&mut events).await;
    assert_eq!(deltas(&all), [200, 50]);
    assert_eq!(events.usage().input_tokens, 250);
    let Some(Event::Finished { usage_totals, .. }) = all.last() else {
        panic!("{all:?}")
    };
    assert_eq!(usage_totals, &[opus(1250)]);
}

#[tokio::test]
async fn a_backend_without_follow_ups_refuses_them_and_closes_stdin() {
    let backend = backend("follow-up").without_follow_ups();
    assert!(!backend.capabilities().follow_ups);
    let Started { run, mut events } = launch(&backend, request(&root())).await;
    assert_eq!(
        run.send(FollowUp {
            turn_id: TurnId::generate(),
            text: "hi".into()
        }),
        Err(SendError::Unsupported)
    );
    // The script's read sees end of file at once, so it exits after its first turn.
    let all = rest(&mut events).await;
    assert_eq!(
        outcome(&all),
        &Outcome::Completed {
            result: Some("First answer.".into())
        }
    );
}

#[tokio::test]
async fn the_resume_id_reaches_the_cli() {
    let mut request = request(&root());
    request.resume = Some(Resume::new("sess-42.b_c"));
    let mut events = launch(&backend("resume"), request.clone()).await.events;
    let first = next(&mut events).await;
    assert_eq!(
        first,
        Event::SessionStarted {
            session_id: "sess-42.b_c".into(),
            model: None,
            api_key_source: None
        }
    );
    rest(&mut events).await;

    request.resume = None;
    let mut events = launch(&backend("resume"), request).await.events;
    assert!(matches!(
        next(&mut events).await,
        Event::SessionStarted { session_id, .. } if session_id == "fresh-session"
    ));
}

#[tokio::test]
async fn malformed_and_oversized_lines_are_skipped_with_warnings() {
    let backend = backend("malformed").with_limits(OutputLimits {
        max_line_bytes: 1024,
        ..OutputLimits::default()
    });
    let mut events = launch(&backend, request(&root())).await.events;
    let all = rest(&mut events).await;
    let warnings: Vec<(WarningKind, &str)> = all
        .iter()
        .filter_map(|event| match event {
            Event::Warning { warning, detail } => Some((*warning, detail.as_str())),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 4, "{all:?}");
    assert_eq!(warnings[0].0, WarningKind::MalformedLine);
    assert_eq!(warnings[1].0, WarningKind::UnknownEvent);
    assert_eq!(warnings[2].0, WarningKind::MalformedLine);
    assert_eq!(
        warnings[3],
        (WarningKind::OversizedLine, "skipped a 5000-byte line")
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
async fn a_crashed_cli_fails_the_run_with_its_stderr() {
    let mut events = launch(&backend("crash"), request(&root())).await.events;
    let all = rest(&mut events).await;
    let Outcome::Failed(failure) = outcome(&all) else {
        panic!("{all:?}")
    };
    assert_eq!(failure.failure, FailureKind::Crashed);
    assert_eq!(failure.exit.unwrap().signal, Some(9));
    assert_eq!(
        failure.stderr_tail.as_deref(),
        Some("fatal: the fake CLI fell over")
    );
    assert_eq!(failure.message, "the CLI was killed by signal 9");
}

#[tokio::test]
async fn an_unsuccessful_exit_without_a_result_fails_the_run() {
    let mut events = launch(&backend("exit-code"), request(&root())).await.events;
    let all = rest(&mut events).await;
    let Outcome::Failed(failure) = outcome(&all) else {
        panic!("{all:?}")
    };
    assert_eq!(failure.failure, FailureKind::Crashed);
    assert_eq!(failure.exit.unwrap().code, Some(2));
    assert_eq!(failure.stderr_tail.as_deref(), Some("error: not logged in"));
}

#[tokio::test]
async fn an_outcome_the_cli_reports_wins_over_its_exit_code() {
    let mut events = launch(&backend("vendor-error"), request(&root()))
        .await
        .events;
    let all = rest(&mut events).await;
    let Outcome::Failed(failure) = outcome(&all) else {
        panic!("{all:?}")
    };
    assert_eq!(failure.failure, FailureKind::RateLimited);
    assert_eq!(failure.message, "weekly limit reached");
    assert!(matches!(
        &all[1],
        Event::RateLimit(window) if window.status == LimitStatus::Rejected
    ));
}

#[tokio::test]
async fn dropping_the_run_and_its_stream_stops_the_cli() {
    let Started { run, mut events } = launch(&backend("hang"), request(&root())).await;
    next(&mut events).await;
    let Event::Text { text: pid, .. } = next(&mut events).await else {
        panic!("no pid")
    };
    drop(events);
    drop(run);
    wait_until_gone(&pid).await;
}

#[tokio::test]
async fn bad_requests_are_refused_before_spawning() {
    let mut bad = request(&root());
    bad.prompt = String::new();
    assert!(matches!(
        backend("resume").start(bad),
        Err(StartError::Invalid(_))
    ));
    let mut bad = request(&root());
    bad.resume = Some(Resume::new("x\"; rm -rf /"));
    assert!(matches!(
        backend("resume").start(bad),
        Err(StartError::Invalid(_))
    ));
    let bad = request(Path::new("/nonexistent-wisp-worktree"));
    let error = backend("resume").start(bad).unwrap_err();
    assert!(matches!(error, StartError::Spawn(_)), "{error:?}");
    let script = Script {
        steps: vec![Step::Init {
            session_id: "$(touch /tmp/pwned)".into(),
            model: None,
        }],
    };
    assert!(matches!(
        FakeBackend::new(launcher(), script).start(request(&root())),
        Err(StartError::Invalid(_))
    ));
}

#[tokio::test]
async fn backends_work_behind_trait_objects() {
    let backends: Vec<Arc<dyn Backend>> = vec![
        Arc::new(backend("resume")),
        Arc::new(backend("resume").without_follow_ups()),
    ];
    for backend in backends {
        assert_eq!(backend.name(), "fake");
        let mut events = launch(&*backend, request(&root())).await.events;
        let all = rest(&mut events).await;
        assert!(matches!(outcome(&all), Outcome::Completed { .. }));
    }
}

async fn wait_until_gone(pid: &str) {
    let pid = Pid::from_raw(pid.parse().unwrap()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while rustix::process::test_kill_process(pid).is_ok() {
        assert!(Instant::now() < deadline, "the fake CLI {pid:?} survived");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
