//! `plxd mcp`, the coordinator's Parallax tools (#195, decision 0019): the built binary, speaking
//! MCP on stdio, against an in-process plxd whose workers run on the fake backend in a real git
//! repository.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use parallax_protocol::methods::{AgentList, AgentStart, ContextList, ContextRead, ProjectStart};
use parallax_protocol::{
    AccountChoice, AgentListParams, AgentRun, AgentStatus, ContextListParams, ContextReadParams,
    CoordinatorThreadId, ProjectId, ProjectStartParams, RunId,
};
use plxd::backend::fake::Step;
use plxd::mcp::{MAX_CONTEXT_BYTES, MAX_MESSAGE_BYTES, MAX_PATH_BYTES, MAX_TEXT_BYTES, TOOLS};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::time::{Instant, sleep, timeout};

use crate::agents::{Conn, Host, create, end_turn, fake, init, project_params, start_params, text};
use crate::support::{PATIENCE, temp_dir};

/// A worker that edits the README, says so, waits for one message, echoes it, and finishes.
fn worker() -> Vec<Step> {
    vec![
        init("session-1"),
        Step::WriteFile {
            path: "README.md".to_owned(),
            content: "hello\nEdited by a subagent.\n".to_owned(),
        },
        text("Edited the README."),
        Step::EndTurn { result: None },
        Step::AwaitFollowUp,
        end_turn("Done."),
    ]
}

/// `plxd mcp` bound to `project` and `thread`, initialized.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    next_id: i64,
}

fn command(data_dir: &Path, project: ProjectId, thread: CoordinatorThreadId) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_plxd"));
    command
        .args(["mcp", "--data-dir"])
        .arg(data_dir)
        .args(["--project", &project.to_string()])
        .args(["--coordinator-thread", &thread.to_string()])
        .env_remove("PLXD_DATA_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

impl Mcp {
    async fn start(data_dir: &Path, project: ProjectId, thread: CoordinatorThreadId) -> Self {
        let mut child = command(data_dir, project, thread)
            .spawn()
            .expect("spawn plxd mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut mcp = Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        let initialized = mcp
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "plxd-tests", "version": "0.0.0"},
                }),
            )
            .await;
        assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(initialized["result"]["serverInfo"]["name"], "plxd");
        assert!(initialized["result"]["capabilities"]["tools"].is_object());
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        mcp
    }

    async fn send(&mut self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await.unwrap();
    }

    async fn read(&mut self) -> Option<Value> {
        let line = timeout(PATIENCE, self.stdout.next_line())
            .await
            .expect("a line from plxd mcp")
            .unwrap()?;
        Some(serde_json::from_str(&line).expect("a JSON line"))
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        let response = self.read().await.expect("a response");
        assert_eq!(response["id"], id, "{response}");
        response
    }

    /// Calls a tool and returns its text and whether it is an error.
    async fn tool(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let response = self
            .request("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        let result = &response["result"];
        assert!(result.is_object(), "{response}");
        (
            result["content"][0]["text"].as_str().unwrap().to_owned(),
            result["isError"].as_bool().unwrap(),
        )
    }

    /// Calls a tool that must succeed and parses its JSON text.
    async fn ok(&mut self, name: &str, arguments: Value) -> Value {
        let (text, is_error) = self.tool(name, arguments).await;
        assert!(!is_error, "{name} failed: {text}");
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("{name} returned {text}"))
    }

    /// Calls a tool that must fail, and returns its message.
    async fn refused(&mut self, name: &str, arguments: Value) -> String {
        let (text, is_error) = self.tool(name, arguments).await;
        assert!(is_error, "{name} succeeded: {text}");
        text
    }

    /// Polls `agent_status` until `done` holds.
    async fn status_until(&mut self, run_id: &str, done: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let status = self.ok("agent_status", json!({"runId": run_id})).await;
            if done(&status) {
                return status;
            }
            assert!(Instant::now() < deadline, "gave up; last status {status}");
            sleep(Duration::from_millis(50)).await;
        }
    }
}

async fn runs(client: &mut Conn, project: ProjectId) -> Vec<AgentRun> {
    client
        .call::<AgentList>(AgentListParams {
            project: Some(project),
        })
        .await
        .unwrap()
        .runs
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_coordinator_spawns_steers_reviews_and_records_through_its_tools() {
    let host = Host::start(temp_dir(), fake(worker()));
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    let thread = CoordinatorThreadId::generate();
    let mut mcp = Mcp::start(host.dir.path(), project.id, thread).await;

    let listed = mcp.request("tools/list", json!({})).await;
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, TOOLS);
    assert_eq!(mcp.request("ping", json!({})).await["result"], json!({}));

    let spawned = mcp
        .ok(
            "spawn_agent",
            json!({"prompt": "Edit the README.", "account": {"kind": "subscription", "backend": "fake"}}),
        )
        .await;
    assert_eq!(spawned["startedByThisCoordinator"], true);
    let run_id = spawned["runId"].as_str().unwrap().to_owned();
    let stored = runs(&mut client, project.id).await;
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id.to_string(), run_id);
    assert_eq!(
        stored[0].coordinator_thread,
        Some(thread),
        "the run is tagged with the coordinator thread that spawned it"
    );

    let listed = mcp.ok("list_agents", json!({})).await;
    assert_eq!(listed["runs"][0]["runId"], run_id.as_str());
    let working = mcp
        .status_until(&run_id, |status| {
            status["lastOutput"] == "Edited the README."
        })
        .await;
    assert_eq!(working["run"]["status"], "running");

    mcp.ok(
        "message_agent",
        json!({"runId": run_id, "text": "Wrap up."}),
    )
    .await;
    let done = mcp
        .status_until(&run_id, |status| status["run"]["status"] == "completed")
        .await;
    assert_eq!(done["lastOutput"], "Done.");
    assert_eq!(done["run"]["diff"]["files"], 1);
    assert_eq!(done["run"]["diff"]["insertions"], 1);

    let (diff, is_error) = mcp.tool("agent_diff", json!({"runId": run_id})).await;
    assert!(!is_error, "{diff}");
    assert!(diff.contains("1 files changed, +1 -0"), "{diff}");
    assert!(
        diff.contains("diff --git a/README.md b/README.md"),
        "{diff}"
    );
    assert!(diff.contains("+Edited by a subagent."), "{diff}");

    let second = mcp
        .ok(
            "spawn_agent",
            json!({"prompt": "Wait.", "account": {"kind": "subscription", "backend": "fake"}}),
        )
        .await;
    let second = second["runId"].as_str().unwrap().to_owned();
    mcp.status_until(&second, |status| status["run"]["status"] == "running")
        .await;
    mcp.ok("cancel_agent", json!({"runId": second})).await;
    mcp.status_until(&second, |status| status["run"]["status"] == "cancelled")
        .await;

    let written = mcp
        .ok(
            "write_context",
            json!({"path": "plan.md", "content": "# Plan\n1. Edit the README.\n"}),
        )
        .await;
    assert_eq!(written["path"], "plan.md");
    assert_eq!(written["lastWriter"], "coordinator");
    let files = mcp.ok("read_context", json!({})).await;
    assert_eq!(files["files"][0]["path"], "plan.md");
    let (content, is_error) = mcp.tool("read_context", json!({"path": "plan.md"})).await;
    assert!(!is_error);
    assert_eq!(content, "# Plan\n1. Edit the README.\n");
    let read = client
        .call::<ContextRead>(ContextReadParams {
            project: project.id,
            path: "plan.md".to_owned(),
        })
        .await
        .unwrap();
    assert_eq!(
        read.content, content,
        "the note is in the project's own context"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_tools_reach_only_the_bound_project() {
    let host = Host::start(temp_dir(), fake(worker()));
    let mut client = host.client().await;
    let dir = host.dir.path();
    let ours = create(&mut client, project_params(&dir.join("ours"))).await;
    let theirs = create(&mut client, project_params(&dir.join("theirs"))).await;
    let their_run = client
        .call::<AgentStart>(start_params(theirs.id, "Their task."))
        .await
        .unwrap()
        .run;
    let their_id = their_run.id.to_string();
    let mut mcp = Mcp::start(dir, ours.id, CoordinatorThreadId::generate()).await;

    let listed = mcp.ok("list_agents", json!({})).await;
    assert_eq!(
        listed["runs"],
        json!([]),
        "another project's runs never show"
    );
    for tool in ["agent_status", "cancel_agent", "agent_diff"] {
        let refused = mcp.refused(tool, json!({"runId": their_id})).await;
        assert!(
            refused.contains("this project has no agent run"),
            "{tool}: {refused}"
        );
    }
    let refused = mcp
        .refused("message_agent", json!({"runId": their_id, "text": "Stop."}))
        .await;
    assert!(
        refused.contains("this project has no agent run"),
        "{refused}"
    );
    let unknown = mcp
        .refused(
            "agent_status",
            json!({"runId": RunId::generate().to_string()}),
        )
        .await;
    assert!(
        unknown.contains("this project has no agent run"),
        "{unknown}"
    );

    for (tool, arguments) in [
        (
            "spawn_agent",
            json!({"prompt": "Sneak in.", "project": theirs.id}),
        ),
        ("list_agents", json!({"project": theirs.id})),
        ("read_context", json!({"project": theirs.id})),
        (
            "write_context",
            json!({"path": "x.md", "content": "x", "project": theirs.id}),
        ),
        (
            "spawn_agent",
            json!({"prompt": "Retag.", "coordinatorThread": CoordinatorThreadId::generate()}),
        ),
    ] {
        let refused = mcp.refused(tool, arguments).await;
        assert!(refused.contains("unknown field"), "{tool}: {refused}");
    }

    mcp.ok(
        "write_context",
        json!({"path": "plan.md", "content": "Ours."}),
    )
    .await;
    let their_files = client
        .call::<ContextList>(ContextListParams { project: theirs.id })
        .await
        .unwrap()
        .files;
    assert!(their_files.is_empty(), "{their_files:?}");

    let their_runs = runs(&mut client, theirs.id).await;
    assert_eq!(their_runs.len(), 1, "nothing was spawned in their project");
    assert_ne!(their_runs[0].status, AgentStatus::Cancelled);
    assert_eq!(their_runs[0].coordinator_thread, None);
    assert!(runs(&mut client, ours.id).await.is_empty());

    let output = command(dir, ProjectId::generate(), CoordinatorThreadId::generate())
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("plxd has no project"), "{stderr}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tool_inputs_are_size_limited() {
    let host = Host::start(temp_dir(), fake(worker()));
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    let mut mcp = Mcp::start(host.dir.path(), project.id, CoordinatorThreadId::generate()).await;

    let long = "x".repeat(MAX_TEXT_BYTES + 1);
    let refused = mcp.refused("spawn_agent", json!({"prompt": long})).await;
    assert!(refused.contains("at most"), "{refused}");
    assert!(runs(&mut client, project.id).await.is_empty());
    let refused = mcp
        .refused(
            "message_agent",
            json!({"runId": RunId::generate().to_string(), "text": long}),
        )
        .await;
    assert!(refused.contains("at most"), "{refused}");
    let path = format!("{}.md", "p".repeat(MAX_PATH_BYTES));
    let refused = mcp.refused("read_context", json!({"path": path})).await;
    assert!(refused.contains("at most"), "{refused}");
    let content = "x".repeat(MAX_CONTEXT_BYTES + 1);
    let refused = mcp
        .refused(
            "write_context",
            json!({"path": "big.md", "content": content}),
        )
        .await;
    assert!(refused.contains("at most"), "{refused}");

    let unknown = mcp
        .request(
            "tools/call",
            json!({"name": "plan_approve", "arguments": {}}),
        )
        .await;
    assert_eq!(unknown["error"]["code"], -32602, "{unknown}");

    let huge = format!(
        r#"{{"jsonrpc":"2.0","id":99,"method":"tools/call","params":{{"name":"list_agents","arguments":{{"pad":"{}"}}}}}}"#,
        "x".repeat(MAX_MESSAGE_BYTES)
    );
    mcp.stdin.write_all(huge.as_bytes()).await.unwrap();
    mcp.stdin.write_all(b"\n").await.unwrap();
    let answer = mcp.read().await.expect("an error for the oversized line");
    assert_eq!(answer["error"]["code"], -32600, "{answer}");
    assert!(answer["id"].is_null());
    let status = timeout(PATIENCE, mcp.child.wait()).await.unwrap().unwrap();
    assert!(!status.success(), "the server ends after an oversized line");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_coordinator_never_sees_or_steers_its_own_run() {
    let host = Host::start(temp_dir(), fake(vec![init("coordinator-1"), Step::Hang]));
    let mut client = host.client().await;
    let project = create(&mut client, project_params(host.dir.path())).await;
    let coordinator = client
        .call::<ProjectStart>(ProjectStartParams {
            project: project.id,
            run_id: RunId::generate(),
            prompt: "Plan.".to_owned(),
            account: Some(AccountChoice::Subscription {
                backend: "fake".to_owned(),
            }),
            model: None,
            effort: None,
            permission: None,
            images: Vec::new(),
            approvals: false,
        })
        .await
        .unwrap()
        .run;
    let thread = coordinator.coordinator_thread.unwrap();
    let mut mcp = Mcp::start(host.dir.path(), project.id, thread).await;

    let listed = mcp.ok("list_agents", json!({})).await;
    assert_eq!(listed["runs"], json!([]));
    let refused = mcp
        .refused(
            "message_agent",
            json!({"runId": coordinator.id.to_string(), "text": "Talk to yourself."}),
        )
        .await;
    assert!(
        refused.contains("this project has no agent run"),
        "{refused}"
    );
}
