//! The Cursor backend against a fake `agent` on `PATH` that replays ACP transcripts recorded
//! from Cursor Agent 2026.10.01-14929f9, so every test spawns a real process and speaks the
//! protocol both ways. No test runs the real CLI.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

use super::{BUILD_PLAN, CursorBackend, arguments};
use crate::backend::process::{Environment, Launcher};
use crate::backend::{
    AccountRef, AgentPermission, Answer, ApiKey, Backend, Credential, Decision, Event, EventStream,
    FailureKind, FollowUp, Outcome, Resume, RunId, RunRequest, StartError, ToolPolicy, ToolStatus,
    TurnId,
};
use crate::paths::DataDir;

const FAKE_AGENT: &str = include_str!("fixtures/fake-agent.sh");
const TURN: &str = "01997e2a-4c3b-7d10-8a2e-5f6b7c8d9e01";
const PROMPT: &str = "Make a todo list of two items, then run the shell command `git status --short` \
                      and then create a file hello.txt containing hi. Then reply with one word: done.";

fn fixture(name: &str) -> &'static str {
    match name {
        "approval" => include_str!("fixtures/approval.jsonl"),
        "plan" => include_str!("fixtures/plan.jsonl"),
        "resume" => include_str!("fixtures/resume.jsonl"),
        "not-signed-in" => include_str!("fixtures/not-signed-in.jsonl"),
        "cancel" => include_str!("fixtures/cancel.jsonl"),
        "interrupt" => include_str!("fixtures/interrupt.jsonl"),
        "plan-denied" => include_str!("fixtures/plan-denied.jsonl"),
        "steer" => include_str!("fixtures/steer.jsonl"),
        other => panic!("no fixture {other}"),
    }
}

/// A fake `agent` on the launcher's `PATH`, in a folder that also holds what it records.
struct Fake {
    dir: TempDir,
    backend: CursorBackend,
}

impl Fake {
    fn new(fixture_name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let bin = root.join("bin");
        fs::create_dir(&bin).unwrap();
        let program = bin.join("agent");
        fs::write(&program, FAKE_AGENT).unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
        let path = root.join("fixture.jsonl");
        fs::write(&path, fixture(fixture_name)).unwrap();
        let base: Environment = [
            ("PATH", format!("{}:/usr/bin:/bin", bin.display())),
            ("FAKE_AGENT_DIR", root.display().to_string()),
            ("FAKE_AGENT_FIXTURE", path.display().to_string()),
            ("CURSOR_API_KEY", "parallax-test-not-a-key".into()),
            ("CURSOR_API_ENDPOINT", "https://example.invalid".into()),
        ]
        .into_iter()
        .collect();
        let launcher = Launcher::new(DataDir::new(root.join("data")).unwrap(), base);
        Self {
            dir,
            backend: CursorBackend::new(launcher),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().canonicalize().unwrap()
    }

    fn recorded(&self, name: &str) -> String {
        fs::read_to_string(self.root().join(name)).unwrap_or_default()
    }

    /// What plxd wrote on stdin, one JSON message per line.
    fn stdin(&self) -> Vec<Value> {
        self.recorded("stdin")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn request(&self) -> RunRequest {
        RunRequest {
            run_id: RunId::generate(),
            turn_id: Some(TURN.parse().unwrap()),
            cwd: self.root(),
            prompt: PROMPT.into(),
            images: Vec::new(),
            policy: ToolPolicy::WorkspaceWrite,
            sandbox: None,
            account: AccountRef {
                id: "cursor".into(),
                credential: Credential::Subscription { config_home: None },
            },
            resume: None,
            model: Some("composer-2.5-fast".into()),
            effort: None,
            permission: None,
            context_window: None,
            fast: None,
            coordinator_tools: None,
            approvals: true,
            thread: true,
        }
    }
}

async fn next(events: &mut EventStream) -> Event {
    tokio::time::timeout(Duration::from_secs(10), events.next())
        .await
        .expect("no event within 10 s")
        .expect("the stream ended")
}

/// Every event until `Finished`, answering each permission request with `answer`, and sending
/// `follow_up`, if any, when the first one arrives.
async fn run(
    fake: &Fake,
    request: RunRequest,
    answer: impl Fn(&str) -> Decision,
    mut follow_up: Option<FollowUp>,
) -> Vec<Event> {
    let started = fake.backend.start(request).unwrap();
    let mut events = started.events;
    let mut all = Vec::new();
    loop {
        let event = next(&mut events).await;
        if let Event::ApprovalRequested(request) = &event {
            if let Some(message) = follow_up.take() {
                started.run.send(message).unwrap();
            }
            let decision = answer(&request.tool_name);
            started
                .run
                .answer(Answer {
                    approval_id: request.approval_id,
                    decision,
                })
                .unwrap();
        }
        let terminal = event.is_terminal();
        all.push(event);
        if terminal {
            return all;
        }
    }
}

fn allow(_: &str) -> Decision {
    Decision::Allow {
        input: None,
        always: false,
    }
}

fn outcome(events: &[Event]) -> &Outcome {
    match events.last() {
        Some(Event::Finished { outcome, .. }) => outcome,
        other => panic!("expected Finished last, got {other:?}"),
    }
}

fn turns(events: &[Event]) -> Vec<&Event> {
    events
        .iter()
        .filter(|event| {
            matches!(
                event,
                Event::TurnStarted { .. } | Event::TurnFinished { .. }
            )
        })
        .collect()
}

fn status_of<'a>(events: &'a [Event], call: &str) -> Option<&'a ToolStatus> {
    events.iter().find_map(|event| match event {
        Event::ToolResult {
            call_id, status, ..
        } if call_id == call => Some(status),
        _ => None,
    })
}

/// The approval transcript, its request allowed, with a follow-up sent while it waited.
async fn approved() -> (Fake, Vec<Event>, FollowUp) {
    let fake = Fake::new("approval");
    let follow_up = FollowUp {
        turn_id: TurnId::generate(),
        text: "And one more thing.".into(),
        images: Vec::new(),
        steer: false,
    };
    let events = run(&fake, fake.request(), allow, Some(follow_up.clone())).await;
    (fake, events, follow_up)
}

#[tokio::test]
async fn a_thread_asks_through_plxd_and_takes_follow_ups_in_its_live_session() {
    let (fake, _, _) = approved().await;

    assert_eq!(
        fake.recorded("argv").lines().collect::<Vec<_>>(),
        ["--model", "composer-2.5-fast", "acp"]
    );
    let env = fake.recorded("env");
    assert!(
        !env.contains("CURSOR_API_KEY") && !env.contains("CURSOR_API_ENDPOINT"),
        "{env}"
    );

    let stdin = fake.stdin();
    let methods: Vec<_> = stdin.iter().map(|line| line["method"].as_str()).collect();
    assert_eq!(
        methods,
        [
            Some("initialize"),
            Some("session/new"),
            Some("session/prompt"),
            None,
            None,
            None,
            Some("session/prompt")
        ]
    );
    assert_eq!(stdin[1]["params"]["cwd"], fake.root().to_str().unwrap());
    assert_eq!(
        stdin[2]["params"]["prompt"],
        json!([{"type": "text", "text": PROMPT}]),
        "the first message is the user's own"
    );
    assert_eq!(
        stdin[3],
        json!({"jsonrpc": "2.0", "id": 0, "result": {}}),
        "todos acknowledged"
    );
    assert_eq!(
        stdin[4],
        json!({"jsonrpc": "2.0", "id": 1, "result": {"outcome": {"outcome": "selected", "optionId": "allow-once"}}})
    );
    assert_eq!(
        stdin[6]["params"]["prompt"][0]["text"],
        "And one more thing."
    );
    assert_eq!(
        stdin[6]["params"]["sessionId"],
        "0cb4faa6-1a77-49e3-a4c7-572c1a178f9a"
    );
}

#[tokio::test]
async fn the_stream_becomes_events_the_app_draws() {
    let (_, events, follow_up) = approved().await;
    let session = "0cb4faa6-1a77-49e3-a4c7-572c1a178f9a".to_owned();
    assert_eq!(
        events[0],
        Event::SessionStarted {
            session_id: session,
            model: Some("composer-2.5[fast=true]".into()),
            api_key_source: None,
        }
    );
    let asked = events
        .iter()
        .find_map(|event| match event {
            Event::ApprovalRequested(request) => Some(request),
            _ => None,
        })
        .unwrap();
    assert_eq!(asked.tool_name, "Bash");
    assert_eq!(asked.input, json!({"command": "git status --short"}));
    assert_eq!(
        asked.reason.as_deref(),
        Some("Not in allowlist: git status")
    );
    let call = asked.call_id.clone().unwrap();
    assert!(events.contains(&Event::ToolCall {
        call_id: call.clone(),
        name: "Bash".into(),
        input: json!({"command": "git status --short"}),
    }));
    assert_eq!(status_of(&events, &call), Some(&ToolStatus::Ok));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ToolCall { name, input, .. }
        if name == "Edit" && input["path"] == "/tmp/acp-probe/repo/hello.txt"))
    );
    let reasoning = events
        .iter()
        .filter(|event| matches!(event, Event::Reasoning { .. }))
        .count();
    assert_eq!(
        reasoning, 3,
        "thinking chunks join into one event per run of them"
    );
    assert!(events.contains(&Event::TodoList {
        items: vec![
            crate::backend::TodoItem {
                text: "Run git status --short".into(),
                status: crate::backend::TodoStatus::Completed
            },
            crate::backend::TodoItem {
                text: "Create hello.txt containing hi".into(),
                status: crate::backend::TodoStatus::Completed
            },
        ]
    }));
    let first = Some(TURN.parse().unwrap());
    assert_eq!(
        turns(&events),
        [
            &Event::TurnStarted { turn_id: first },
            &Event::TurnFinished {
                turn_id: first,
                result: Some("done".into())
            },
            &Event::TurnStarted {
                turn_id: Some(follow_up.turn_id)
            },
            &Event::TurnFinished {
                turn_id: Some(follow_up.turn_id),
                result: Some("Also done.".into())
            },
        ]
    );
    assert_eq!(
        outcome(&events),
        &Outcome::Completed {
            result: Some("Also done.".into())
        }
    );
}

#[tokio::test]
async fn a_denied_request_rejects_the_call() {
    let fake = Fake::new("approval");
    let deny = |_: &str| Decision::Deny {
        message: "Not now.".into(),
        interrupt: false,
    };
    let events = run(&fake, fake.request(), deny, None).await;
    assert_eq!(
        fake.stdin()[4],
        json!({"jsonrpc": "2.0", "id": 1, "result": {"outcome": {"outcome": "selected", "optionId": "reject-once"}}})
    );
    assert_eq!(
        status_of(&events, "tool_a9c103b0-e438-4509-a890-8dd71a1d771"),
        Some(&ToolStatus::Denied)
    );
}

#[tokio::test]
async fn without_approvals_plxd_rejects_what_would_ask() {
    let fake = Fake::new("approval");
    let mut request = fake.request();
    request.approvals = false;
    let events = run(&fake, request, |_| unreachable!("nothing asks"), None).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_)))
    );
    assert_eq!(
        fake.stdin()[4]["result"]["outcome"]["optionId"],
        "reject-once"
    );
}

#[tokio::test]
async fn an_approved_plan_is_built_in_the_same_turn() {
    let fake = Fake::new("plan");
    let mut request = fake.request();
    request.permission = Some(AgentPermission::Plan);
    let events = run(&fake, request, allow, None).await;

    let stdin = fake.stdin();
    assert_eq!(stdin[2]["method"], "session/set_mode");
    assert_eq!(stdin[2]["params"]["modeId"], "plan");
    assert_eq!(
        stdin[3]["method"], "session/prompt",
        "the prompt waits for plan mode"
    );
    assert_eq!(
        stdin[4],
        json!({"jsonrpc": "2.0", "id": 0, "result": {"outcome": {"outcome": "accepted"}}})
    );
    assert_eq!(stdin[5]["params"]["modeId"], "agent");
    assert_eq!(
        stdin[6]["params"]["prompt"],
        json!([{"type": "text", "text": BUILD_PLAN}])
    );

    let asked = events
        .iter()
        .find_map(|event| match event {
            Event::ApprovalRequested(request) => Some(request),
            _ => None,
        })
        .unwrap();
    assert_eq!(asked.tool_name, "ExitPlanMode");
    assert!(asked.interactive);
    assert!(
        asked.input["plan"]
            .as_str()
            .unwrap()
            .starts_with("# Add CONTRIBUTING.md")
    );
    let call = asked.call_id.clone().unwrap();
    assert!(events.iter().any(
        |event| matches!(event, Event::ToolCall { call_id, name, input }
        if *call_id == call && name == "ExitPlanMode" && input["plan"] == asked.input["plan"])
    ));
    assert_eq!(status_of(&events, &call), Some(&ToolStatus::Ok));
    let turn = Some(TURN.parse().unwrap());
    assert_eq!(
        turns(&events),
        [
            &Event::TurnStarted { turn_id: turn },
            &Event::TurnFinished {
                turn_id: turn,
                result: Some("Added CONTRIBUTING.md.".into())
            },
        ],
        "the build goes on as the plan's turn"
    );
}

#[tokio::test]
async fn a_denied_plan_is_rejected_with_the_users_reason_and_keeps_planning() {
    let fake = Fake::new("plan-denied");
    let mut request = fake.request();
    request.permission = Some(AgentPermission::Plan);
    let deny = |_: &str| Decision::Deny {
        message: "Use three sections instead".into(),
        interrupt: false,
    };
    let events = run(&fake, request, deny, None).await;

    let stdin = fake.stdin();
    assert_eq!(
        stdin[4],
        json!({"jsonrpc": "2.0", "id": 0, "result": {"outcome": {"outcome": "rejected", "reason": "Use three sections instead"}}})
    );
    assert_eq!(stdin.len(), 5, "no switch to agent mode, and no build");
    assert_eq!(
        status_of(&events, "tool_6d56f15f-9285-4e66-8f3a-de596a50152"),
        Some(&ToolStatus::Denied)
    );
    let turn = Some(TURN.parse().unwrap());
    assert_eq!(
        turns(&events),
        [
            &Event::TurnStarted { turn_id: turn },
            &Event::TurnFinished {
                turn_id: turn,
                result: Some("Planning three sections instead.".into())
            },
        ]
    );
}

#[tokio::test]
async fn an_interrupting_denial_cancels_the_turn_and_withdraws_the_other_request() {
    let fake = Fake::new("interrupt");
    let started = fake.backend.start(fake.request()).unwrap();
    let mut events = started.events;
    let mut asked = Vec::new();
    while asked.len() < 2 {
        if let Event::ApprovalRequested(request) = next(&mut events).await {
            asked.push(request.approval_id);
        }
    }
    let deny = Decision::Deny {
        message: "Stopped.".into(),
        interrupt: true,
    };
    started
        .run
        .answer(Answer {
            approval_id: asked[0],
            decision: deny,
        })
        .unwrap();
    let mut rest = Vec::new();
    while !rest.last().is_some_and(Event::is_terminal) {
        rest.push(next(&mut events).await);
    }

    let stdin = fake.stdin();
    assert_eq!(stdin[3]["result"]["outcome"]["optionId"], "reject-once");
    assert_eq!(stdin[4]["method"], "session/cancel");
    assert_eq!(
        stdin[5],
        json!({"jsonrpc": "2.0", "id": 1, "result": {"outcome": {"outcome": "cancelled"}}})
    );
    assert!(rest.contains(&Event::ApprovalWithdrawn {
        approval_id: asked[1]
    }));
    assert!(
        rest.iter()
            .any(|event| matches!(event, Event::TurnFinished { .. }))
    );
}

#[tokio::test]
async fn cancel_stops_cursor_mid_turn() {
    let fake = Fake::new("cancel");
    let started = fake.backend.start(fake.request()).unwrap();
    let mut events = started.events;
    while !matches!(next(&mut events).await, Event::TextDelta { .. }) {}
    started.run.cancel();
    let mut rest = Vec::new();
    while !rest.last().is_some_and(Event::is_terminal) {
        rest.push(next(&mut events).await);
    }
    assert_eq!(outcome(&rest), &Outcome::Cancelled);
}

#[tokio::test]
async fn a_resumed_session_loads_and_drops_its_replayed_history() {
    let fake = Fake::new("resume");
    let mut request = fake.request();
    request.prompt =
        "Which file did you create earlier in this chat? Answer with the file name only.".into();
    request.resume = Some(Resume::new("0cb4faa6-1a77-49e3-a4c7-572c1a178f9a"));
    let events = run(&fake, request, allow, None).await;

    let stdin = fake.stdin();
    assert_eq!(stdin[1]["method"], "session/load");
    assert_eq!(
        stdin[1]["params"]["sessionId"],
        "0cb4faa6-1a77-49e3-a4c7-572c1a178f9a"
    );
    assert_eq!(
        stdin[2]["params"]["sessionId"],
        "0cb4faa6-1a77-49e3-a4c7-572c1a178f9a"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::ToolCall { .. } | Event::TodoList { .. })),
        "the replay's tool calls are already in the run's log"
    );
    let text: String = events
        .iter()
        .filter_map(|event| match event {
            Event::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "hello.txt");
    assert_eq!(
        outcome(&events),
        &Outcome::Completed {
            result: Some("hello.txt".into())
        }
    );
}

#[tokio::test]
async fn a_signed_out_cursor_fails_as_not_signed_in() {
    let fake = Fake::new("not-signed-in");
    let events = run(&fake, fake.request(), allow, None).await;
    let Outcome::Failed(failure) = outcome(&events) else {
        panic!("expected a failure");
    };
    assert_eq!(
        failure.failure,
        FailureKind::NotSignedIn,
        "routing falls back on it (0012)"
    );
}

#[test]
fn only_a_threads_subscription_runs_on_cursor() {
    let fake = Fake::new("approval");
    let mut worker = fake.request();
    worker.thread = false;
    assert!(matches!(
        arguments(&worker),
        Err(StartError::Unsupported(_))
    ));
    let mut key = fake.request();
    key.account.credential = Credential::ApiKey(ApiKey::new("k".into()));
    assert!(matches!(arguments(&key), Err(StartError::Unsupported(_))));
    let mut auto = fake.request();
    auto.permission = Some(AgentPermission::Auto);
    assert!(matches!(arguments(&auto), Err(StartError::Unsupported(_))));
    let mut bypass = fake.request();
    bypass.permission = Some(AgentPermission::Bypass);
    bypass.model = None;
    assert_eq!(arguments(&bypass).unwrap(), ["--force", "acp"]);
}

/// PLX-370: ACP can't add to a running turn, so a steer cancels it, and goes as the next prompt
/// once the cancelled turn has ended.
#[tokio::test]
async fn a_steer_cancels_the_running_turn_and_goes_next() {
    let fake = Fake::new("steer");
    let started = fake.backend.start(fake.request()).unwrap();
    let mut stream = started.events;
    let mut events = Vec::new();
    loop {
        let event = next(&mut stream).await;
        let running = matches!(&event, Event::ToolCall { call_id, .. } if call_id == "tool_s");
        events.push(event);
        if running {
            break;
        }
    }
    let steer = TurnId::generate();
    started
        .run
        .send(FollowUp {
            turn_id: steer,
            text: "Change of plan: reply BANANA instead.".into(),
            images: Vec::new(),
            steer: true,
        })
        .unwrap();
    loop {
        let event = next(&mut stream).await;
        let terminal = event.is_terminal();
        events.push(event);
        if terminal {
            break;
        }
    }
    let first = Some(TURN.parse().unwrap());
    let turns: Vec<&Event> = events
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
            &Event::TurnStarted { turn_id: first },
            &Event::TurnFinished {
                turn_id: first,
                result: None
            },
            &Event::TurnStarted {
                turn_id: Some(steer)
            },
            &Event::TurnFinished {
                turn_id: Some(steer),
                result: Some("BANANA".into())
            },
        ]
    );
    let stdin = fake.stdin();
    assert_eq!(
        stdin[3],
        json!({"jsonrpc": "2.0", "method": "session/cancel",
               "params": {"sessionId": "0cb4faa6-1a77-49e3-a4c7-572c1a178f9a"}})
    );
    assert_eq!(stdin[4]["method"], "session/prompt");
    assert_eq!(
        stdin[4]["params"]["prompt"][0]["text"],
        "Change of plan: reply BANANA instead."
    );
}
