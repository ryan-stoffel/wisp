//! What the real-CLI tests share: a fake Messages API on 127.0.0.1 that asks for tool calls, a way
//! to run Claude Code as plxd runs a worker against it, and the Bash call's output.

use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::Path;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use plxd::backend::claude::{TEMP_ENV, arguments, worker_temp};
use plxd::backend::run_temp::{self, RunTemp};
use plxd::backend::{AccountRef, Credential, RunId, RunRequest, ToolPolicy, WorkerSandbox};
use plxd::paths::DataDir;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A worker's request as plxd builds it (0013): a worktree at `worktree` in plxd's data folder
/// `data`, whose repository's git folder is `git_dir`, with `context` writable, and a temp
/// folder made as plxd makes one (RYA-130). It runs the probe that [`run_worker`]'s fake API
/// asks for. Keep the [`RunTemp`] until the run is over.
pub fn worker_request(
    home: &Path,
    data: &Path,
    worktree: &Path,
    git_dir: &Path,
    context: &Path,
) -> (RunRequest, RunTemp) {
    let temp = run_temp::create(&DataDir::new(data).unwrap()).unwrap();
    let canonical = temp.path().canonicalize().unwrap();
    let request = RunRequest {
        run_id: RunId::generate(),
        turn_id: None,
        cwd: worktree.to_owned(),
        prompt: "Run the probe.".into(),
        images: Vec::new(),
        policy: ToolPolicy::WorkspaceWrite,
        sandbox: Some(WorkerSandbox::for_worktree(
            home, data, worktree, git_dir, context, &canonical,
        )),
        account: AccountRef {
            id: "test".into(),
            credential: Credential::Subscription { config_home: None },
        },
        resume: None,
        model: Some("claude-sonnet-4-6".into()),
        effort: None,
        permission: None,
        context_window: None,
        fast: None,
        coordinator_tools: None,
        thread_tools: None,
        approvals: false,
        thread: false,
    };
    (request, temp)
}

/// Runs `claude` with the arguments plxd gives `request`, against a fake Messages API whose one
/// Bash call runs `sh probe.sh` in the worktree. Returns stdout, and stdout with stderr for
/// failure messages. `env` adds the test's own variables, such as its API key, to the worker's
/// environment. Like plxd, it gives the CLI the run's temp folder as `CLAUDE_CODE_TMPDIR`, and
/// leaves `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB` unset for a worker: on Linux it widens the sandbox's
/// writes (RYA-20).
pub async fn run_worker(
    claude: &OsStr,
    request: &RunRequest,
    root: &Path,
    home: &Path,
    env: &[(&str, &str)],
) -> (String, String) {
    let base_url = fake_api(vec![ToolCall::bash("sh probe.sh")]).await;
    let temp = worker_temp(&request.sandbox.as_ref().unwrap().temp).unwrap();
    // The CLI's own `TMPDIR`, plxd's in a real run.
    std::fs::create_dir_all(root.join("tmp")).unwrap();

    let mut child = tokio::process::Command::new(claude)
        .args(arguments(request).unwrap())
        .current_dir(&request.cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TMPDIR", root.join("tmp"))
        .env(TEMP_ENV, temp)
        .env("CLAUDE_CONFIG_DIR", root.join("config"))
        .env("ANTHROPIC_BASE_URL", base_url)
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("CLAUDE_CODE_STARTUP_FAILURE_RESULTS", "1")
        .envs(env.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let prompt = json!({
        "type": "user",
        "message": {"role": "user", "content": request.prompt},
        "parent_tool_use_id": null,
    });
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(format!("{prompt}\n").as_bytes())
        .await
        .unwrap();
    drop(stdin);
    let output = tokio::time::timeout(Duration::from_secs(120), child.wait_with_output())
        .await
        .expect("Claude Code didn't finish within 120 s")
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let transcript = format!("{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    (stdout, transcript)
}

/// The output of the run's Bash call, from its `tool_result` on stdout.
pub fn tool_result(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event["type"] == "user")
        .find_map(|event| {
            let content = event.pointer("/message/content/0/content")?;
            Some(match content {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            })
        })
}

/// A tool call the fake Messages API asks for.
#[derive(Clone, Debug)]
pub struct ToolCall {
    /// The tool, such as `Bash`.
    pub name: &'static str,
    /// Its input.
    pub input: Value,
}

impl ToolCall {
    /// A Bash call that runs `command`.
    pub fn bash(command: &str) -> Self {
        Self {
            name: "Bash",
            input: json!({"command": command, "description": "Probe the sandbox"}),
        }
    }
}

/// Starts a fake Messages API on 127.0.0.1 and returns its base URL. Each turn asks for the next
/// of `calls`, in order, and the turn that carries the last one's result ends the conversation.
/// The first call's id is `toolu_01ParallaxProbe`, the next `toolu_01ParallaxProbe1`, and so on.
pub async fn fake_api(calls: Vec<ToolCall>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(serve(listener, calls.into()));
    base_url
}

/// Answers `POST /v1/messages` with the next of `calls`. Everything else is a 404.
async fn serve(listener: TcpListener, calls: Arc<[ToolCall]>) {
    while let Ok((stream, _)) = listener.accept().await {
        tokio::spawn(answer(stream, Arc::clone(&calls)));
    }
}

async fn answer(mut stream: TcpStream, calls: Arc<[ToolCall]>) -> std::io::Result<()> {
    let mut request = Vec::new();
    let mut chunk = [0; 16 * 1024];
    let body_start = loop {
        if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&request[..body_start]).to_ascii_lowercase();
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0);
    while request.len() < body_start + length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&chunk[..read]);
    }
    let target = head.split_whitespace().nth(1).unwrap_or_default();
    let messages_api = target == "/v1/messages" || target.starts_with("/v1/messages?");
    let response = if head.starts_with("post ") && messages_api {
        let body: Value = serde_json::from_slice(&request[body_start..body_start + length])?;
        let events = messages(&body, &calls);
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n{events}",
            events.len()
        )
    } else {
        "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_owned()
    };
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await
}

/// The streamed reply to a Messages request, as server-sent events: the call after the ones the
/// conversation already has results for, or the end.
fn messages(request: &Value, calls: &[ToolCall]) -> String {
    let answered = request["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|block| block["type"] == "tool_result")
        .count();
    let (block, delta, stop) = match calls.get(answered) {
        Some(call) => {
            let id = match answered {
                0 => "toolu_01ParallaxProbe".to_owned(),
                n => format!("toolu_01ParallaxProbe{n}"),
            };
            (
                json!({"type": "tool_use", "id": id, "name": call.name, "input": {}}),
                json!({"type": "input_json_delta", "partial_json": call.input.to_string()}),
                "tool_use",
            )
        }
        None => (
            json!({"type": "text", "text": ""}),
            json!({"type": "text_delta", "text": "done"}),
            "end_turn",
        ),
    };
    let usage = json!({"input_tokens": 1, "output_tokens": 1});
    let message = json!({
        "id": "msg_01ParallaxProbe", "type": "message", "role": "assistant",
        "model": request["model"], "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": usage,
    });
    let events = [
        json!({"type": "message_start", "message": message}),
        json!({"type": "content_block_start", "index": 0, "content_block": block}),
        json!({"type": "content_block_delta", "index": 0, "delta": delta}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null},
               "usage": {"output_tokens": 1}}),
        json!({"type": "message_stop"}),
    ];
    let mut stream = String::new();
    for event in events {
        let kind = event["type"].as_str().unwrap();
        writeln!(stream, "event: {kind}\ndata: {event}\n").unwrap();
    }
    stream
}
