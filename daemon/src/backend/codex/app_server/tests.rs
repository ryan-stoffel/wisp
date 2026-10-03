//! A Codex thread against a fake `codex app-server` on `PATH` that plays a recorded conversation,
//! so each test spawns a real process and reads what plxd wrote to it. No test runs the real CLI.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

use crate::backend::codex::CodexBackend;
use crate::backend::process::{Environment, Launcher};
use crate::backend::{
    AccountRef, AgentEffort, AgentPermission, Answer, ApiKey, Backend, Credential, Decision, Event,
    EventStream, FollowUp, ModelUsage, Outcome, Resume, RunId, RunRequest, StartError, ToolPolicy,
    ToolStatus, TurnId, Usage,
};
use crate::paths::DataDir;

/// A fake `codex` playing `fixture`, and the folder it records into.
fn fake(fixture: &str) -> (TempDir, CodexBackend) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let bin = root.join("bin");
    fs::create_dir(&bin).unwrap();
    let program = bin.join("codex");
    fs::write(&program, include_str!("../fixtures/fake-app-server.sh")).unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
    let conversation = root.join("conversation.jsonl");
    fs::write(&conversation, fixture).unwrap();
    let base: Environment = [
        ("PATH", format!("{}:/usr/bin:/bin", bin.display())),
        ("FAKE_CODEX_DIR", root.display().to_string()),
        ("FAKE_CODEX_FIXTURE", conversation.display().to_string()),
        ("CODEX_API_KEY", "parallax-test-not-a-key".to_owned()),
    ]
    .into_iter()
    .collect();
    let launcher = Launcher::new(DataDir::new(root.join("data")).unwrap(), base);
    (dir, CodexBackend::new(launcher))
}

/// The JSON lines plxd wrote to the fake's stdin.
fn written(dir: &TempDir) -> Vec<Value> {
    fs::read_to_string(dir.path().join("stdin"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn request(permission: AgentPermission) -> RunRequest {
    RunRequest {
        run_id: RunId::generate(),
        turn_id: Some(TurnId::generate()),
        cwd: PathBuf::from("/"),
        prompt: "Commit the README change.".into(),
        images: Vec::new(),
        policy: ToolPolicy::WorkspaceWrite,
        sandbox: None,
        account: AccountRef {
            id: "codex".into(),
            credential: Credential::Subscription { config_home: None },
        },
        resume: None,
        model: Some("gpt-6-sol".into()),
        effort: Some(AgentEffort::High),
        permission: Some(permission),
        context_window: None,
        fast: None,
        coordinator_tools: None,
        thread_tools: None,
        approvals: true,
        thread: true,
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
            return all;
        }
    }
}

#[tokio::test]
async fn an_approval_round_trips_and_a_follow_up_joins_the_live_thread() {
    let (dir, backend) = fake(include_str!("../fixtures/app-server-manual.jsonl"));
    let request = request(AgentPermission::Manual);
    let first = request.turn_id;
    let started = backend.start(request).unwrap();
    let mut stream = started.events;
    let mut events = Vec::new();
    let asked = loop {
        match next(&mut stream).await {
            Event::ApprovalRequested(asked) => break asked,
            event => events.push(event),
        }
    };
    assert_eq!(asked.tool_name, "command_execution");
    assert_eq!(asked.call_id.as_deref(), Some("exec-c"));
    // Sent while the first turn waits: it runs once that turn completes.
    let follow_up = TurnId::generate();
    started
        .run
        .send(FollowUp {
            turn_id: follow_up,
            text: "What's the hash?".into(),
            images: Vec::new(),
        })
        .unwrap();
    started
        .run
        .answer(Answer {
            approval_id: asked.approval_id,
            decision: Decision::Allow {
                input: None,
                always: true,
            },
        })
        .unwrap();
    events.extend(rest(&mut stream).await);

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
                result: Some("Committed.".into())
            },
            &Event::TurnStarted {
                turn_id: Some(follow_up)
            },
            &Event::TurnFinished {
                turn_id: Some(follow_up),
                result: Some("42ed0b6".into())
            },
        ]
    );
    assert!(events.contains(&Event::SessionStarted {
        session_id: "t-1".into(),
        model: Some("gpt-6-sol".into()),
        api_key_source: None,
    }));
    assert!(events.iter().any(|event| matches!(
        event,
        Event::ToolResult { call_id, status: ToolStatus::Ok, .. } if call_id == "exec-c"
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::ApprovalWithdrawn { .. })),
        "an answered request's serverRequest/resolved withdraws nothing"
    );
    assert_eq!(
        events.last(),
        Some(&Event::Finished {
            outcome: Outcome::Completed {
                result: Some("42ed0b6".into())
            },
            usage_totals: vec![ModelUsage {
                model: None,
                usage: Usage {
                    input_tokens: 120,
                    output_tokens: 20,
                    cache_read_tokens: 90,
                    ..Usage::default()
                }
            }],
        })
    );

    assert_manual_writes(&dir);
}

/// What plxd wrote to the Manual thread's app-server, and how it started it.
fn assert_manual_writes(dir: &TempDir) {
    let written = written(dir);
    let methods: Vec<&str> = written
        .iter()
        .map(|line| line["method"].as_str().unwrap_or("(response)"))
        .collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "initialized",
            "thread/start",
            "turn/start",
            "(response)",
            "turn/start"
        ]
    );
    assert_eq!(
        written[2]["params"],
        json!({"cwd": "/", "approvalPolicy": "untrusted", "sandbox": "workspace-write",
               "model": "gpt-6-sol"}),
        "the user's own config, with only the mode and model on top"
    );
    assert_eq!(
        written[3]["params"],
        json!({"threadId": "t-1", "effort": "high", "input": [
            {"type": "text", "text": "Commit the README change.", "text_elements": []}
        ]}),
        "the first message is the user's, as written"
    );
    assert_eq!(
        written[4],
        json!({"id": 0, "result": {"decision": "acceptForSession"}})
    );
    assert_eq!(written[5]["params"]["input"][0]["text"], "What's the hash?");
    let argv = fs::read_to_string(dir.path().join("argv")).unwrap();
    assert_eq!(argv, "app-server\n");
    let env = fs::read_to_string(dir.path().join("env")).unwrap();
    assert!(
        !env.contains("CODEX_API_KEY"),
        "inherited credentials are scrubbed"
    );
}

#[tokio::test]
async fn a_resumed_thread_without_approvals_never_asks_and_declines_what_codex_does() {
    let (dir, backend) = fake(include_str!("../fixtures/app-server-resume.jsonl"));
    let mut request = request(AgentPermission::Manual);
    request.approvals = false;
    request.context_window = Some(872_000);
    request.fast = Some(true);
    request.resume = Some(Resume {
        session_id: "t-0".into(),
        usage_totals: vec![ModelUsage {
            model: None,
            usage: Usage {
                input_tokens: 1000,
                output_tokens: 100,
                ..Usage::default()
            },
        }],
    });
    let events = rest(&mut backend.start(request).unwrap().events).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::ApprovalRequested(_))),
        "a client that can't answer never sees a request"
    );
    assert!(events.iter().any(|event| matches!(
        event,
        Event::ToolResult {
            status: ToolStatus::Denied,
            ..
        }
    )));
    let added: u64 = events
        .iter()
        .filter_map(|event| match event {
            Event::Usage(delta) => Some(delta.usage.input_tokens),
            _ => None,
        })
        .sum();
    assert_eq!(added, 300, "only what the resumed thread added");
    assert!(matches!(
        events.last(),
        Some(Event::Finished {
            outcome: Outcome::Completed { result: None },
            ..
        })
    ));

    let written = written(&dir);
    assert_eq!(written[2]["method"], "thread/resume");
    assert_eq!(
        written[2]["params"],
        json!({"threadId": "t-0", "excludeTurns": true, "cwd": "/", "approvalPolicy": "never",
               "sandbox": "workspace-write", "model": "gpt-6-sol",
               "config": {"model_context_window": 872_000}, "serviceTier": "priority"})
    );
    assert_eq!(
        written[4],
        json!({"id": 0, "result": {"decision": "decline"}})
    );
    assert_eq!(written[5]["id"], 1);
    assert_eq!(written[5]["error"]["code"], -32601);
}

#[test]
fn a_thread_on_an_api_key_or_in_plan_is_refused_before_spawning() {
    let (_dir, backend) = fake("");
    let mut keyed = request(AgentPermission::Edit);
    keyed.account.credential = Credential::ApiKey(ApiKey::new("sk-test".into()));
    let plan = request(AgentPermission::Plan);
    for request in [keyed, plan] {
        assert!(matches!(
            backend.start(request),
            Err(StartError::Unsupported(_))
        ));
    }
}
