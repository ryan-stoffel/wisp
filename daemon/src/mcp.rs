//! `plxd mcp`: the coordinator's Parallax tools, as an MCP server on stdio (#195, decision 0019),
//! and a normal thread's host-wide ones ([`thread`], 0041).
//!
//! The coordinator's CLI launches it with `--project` and `--coordinator-thread`, which plxd
//! writes into the CLI's `--mcp-config` ([`crate::backend::CoordinatorTools`]). Those bind every
//! tool to one project and one thread: no tool takes either as an argument, and tool arguments
//! reject fields they don't know, so the model can't pick another project's runs or context. A
//! thread's CLI launches it with `--thread` instead ([`crate::backend::ThreadTools`]).
//!
//! MCP's stdio transport is JSON-RPC 2.0 as newline-delimited JSON, the same framing as plxd's
//! own protocol (0007), so both sides use `parallax_protocol`'s codec and envelope. Each tool call
//! opens its own connection to plxd's socket, initializes, and makes one or two calls, so the
//! server needs no heartbeat and outlives a plxd restart between calls. It never starts plxd:
//! the coordinator it serves is plxd's own child.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use parallax_protocol::framing::{FrameCodec, FrameError};
use parallax_protocol::jsonrpc::{
    ErrorObject, INVALID_REQUEST, Message, Request, RequestId, Response,
};
use parallax_protocol::methods::{
    AgentCancel, AgentDiff, AgentEvents, AgentList, AgentSend, AgentStart, ContextList,
    ContextRead, ContextWrite, Initialize, ProjectList, RequestMethod,
};
use parallax_protocol::{
    AccountChoice, AgentCancelParams, AgentDiffParams, AgentDiffResult, AgentEventsParams,
    AgentListParams, AgentOutcome, AgentOutputItem, AgentPolicy, AgentRun, AgentSendParams,
    AgentStartParams, Capabilities, ClientInfo, ContextListParams, ContextReadParams,
    ContextWriteId, ContextWriteParams, CoordinatorThreadId, InitializeParams, ParallaxEvent,
    ProjectId, ProjectListParams, ProtocolRange, RunId, TurnId,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{Framed, FramedRead, FramedWrite};

use crate::transport::{self, Stream};

pub mod thread;

/// The server's name in the coordinator's `--mcp-config`, which prefixes its tools' names there.
pub const SERVER: &str = "plxd";

/// Every tool the server offers.
pub const TOOLS: &[&str] = &[
    "spawn_agent",
    "list_agents",
    "agent_status",
    "message_agent",
    "cancel_agent",
    "agent_diff",
    "read_context",
    "write_context",
];

/// [`TOOLS`] as Claude Code names them, `mcp__<server>__<tool>`: the start of a coordinator's
/// `--allowedTools`, so they run without asking in every permission mode (0027). Claude Code's
/// todo tools follow them there (`backend::claude::TODO_TOOLS`, RYA-249).
pub const ALLOWED_TOOLS: &[&str] = &[
    "mcp__plxd__spawn_agent",
    "mcp__plxd__list_agents",
    "mcp__plxd__agent_status",
    "mcp__plxd__message_agent",
    "mcp__plxd__cancel_agent",
    "mcp__plxd__agent_diff",
    "mcp__plxd__read_context",
    "mcp__plxd__write_context",
];

/// The longest line the server reads from the CLI. A longer one gets an error and ends the
/// server, since the stream can't be trusted to resynchronize after it.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// The longest task prompt or message, in bytes.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;

/// The longest shared context path, in bytes.
pub const MAX_PATH_BYTES: usize = 255;

/// The largest shared context file, in bytes: plxd's own per-file cap (0005).
pub const MAX_CONTEXT_BYTES: usize = 1024 * 1024;

/// The most text one tool result carries, in bytes. The rest is cut, with a note.
pub const MAX_RESULT_BYTES: usize = 256 * 1024;

/// How much of a run's prompt `list_agents` and the other run summaries show.
const PROMPT_PREVIEW_BYTES: usize = 500;

/// How much of a run's last output `agent_status` shows: its end.
const LAST_OUTPUT_BYTES: usize = 8 * 1024;

/// MCP protocol versions the server answers with, newest last. A client asking for another gets
/// the newest; the tools use nothing that differs between them.
const MCP_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// What one server is bound to.
#[derive(Clone, Debug)]
pub struct Binding {
    /// plxd's socket.
    pub socket: PathBuf,
    /// The only project the tools reach.
    pub project: ProjectId,
    /// The coordinator thread that spawned runs are tagged with.
    pub thread: CoordinatorThreadId,
}

/// Checks that the bound project exists, then serves MCP on `input` and `output` until `input`
/// ends.
///
/// # Errors
///
/// When plxd can't be reached or doesn't know the project, when a line is longer than
/// [`MAX_MESSAGE_BYTES`], or when reading or writing fails.
pub async fn run(
    binding: &Binding,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
) -> Result<(), String> {
    let mut plxd = Plxd::open(&binding.socket).await?;
    let projects = plxd.call::<ProjectList>(ProjectListParams {}).await?;
    if !projects.projects.iter().any(|p| p.id == binding.project) {
        return Err(format!("plxd has no project {}", binding.project));
    }
    drop(plxd);
    serve(binding, input, output).await
}

/// One server's tools: what `tools/list` shows, and how `tools/call` runs one.
trait Tools {
    /// Every tool's name, as [`Tools::definitions`] lists them.
    fn names(&self) -> &'static [&'static str];
    /// `tools/list`'s `tools`.
    fn definitions(&self) -> Value;
    /// Runs tool `name`, one of [`Tools::names`]: its text, or an error the model sees.
    async fn call(&self, name: &str, arguments: Value) -> Result<String, String>;
}

impl Tools for Binding {
    fn names(&self) -> &'static [&'static str] {
        TOOLS
    }

    fn definitions(&self) -> Value {
        definitions()
    }

    async fn call(&self, name: &str, arguments: Value) -> Result<String, String> {
        call_tool(self, name, arguments).await
    }
}

async fn serve(
    binding: &impl Tools,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
) -> Result<(), String> {
    let mut reader = FramedRead::new(input, FrameCodec::with_max_frame_bytes(MAX_MESSAGE_BYTES));
    let mut writer = FramedWrite::new(output, FrameCodec::new());
    while let Some(frame) = reader.next().await {
        let frame = match frame {
            Ok(frame) => frame,
            Err(FrameError::TooLarge { max_frame_bytes }) => {
                let message = format!("a message is longer than {max_frame_bytes} bytes");
                let error = ErrorObject::new(INVALID_REQUEST, message.clone());
                let _ = writer.send(&Response::error(None, error)).await;
                return Err(message);
            }
            Err(error) => return Err(error.to_string()),
        };
        let response = match Message::from_frame(&frame) {
            Ok(Message::Request(request)) => Response {
                id: Some(request.id.clone()),
                result: answer(binding, &request).await,
            },
            Ok(Message::Notification(_) | Message::Response(_)) => continue,
            Err(malformed) => malformed.into_response(),
        };
        writer
            .send(&response)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn answer(binding: &impl Tools, request: &Request) -> Result<Value, ErrorObject> {
    match request.method.as_str() {
        "initialize" => {
            let asked: InitializeRequest = request.params()?;
            let version = MCP_VERSIONS
                .iter()
                .find(|version| Some(**version) == asked.protocol_version.as_deref())
                .or(MCP_VERSIONS.last())
                .copied();
            Ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": SERVER, "version": crate::version()},
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools": binding.definitions()})),
        "tools/call" => {
            let call: ToolCall = request.params()?;
            if !binding.names().contains(&call.name.as_str()) {
                return Err(ErrorObject::invalid_params(format!(
                    "no tool is named {:?}",
                    call.name
                )));
            }
            let arguments = call.arguments.unwrap_or_else(|| json!({}));
            Ok(tool_result(binding.call(&call.name, arguments).await))
        }
        other => Err(ErrorObject::method_not_found(other)),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeRequest {
    #[serde(default)]
    protocol_version: Option<String>,
}

#[derive(Deserialize)]
struct ToolCall {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

/// A `tools/call` result: the text, and whether it reports a failure the model should see.
fn tool_result(outcome: Result<String, String>) -> Value {
    let (text, is_error) = match outcome {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    json!({
        "content": [{"type": "text", "text": clip(&text, MAX_RESULT_BYTES)}],
        "isError": is_error,
    })
}

/// `text`, cut to at most `max` bytes at a character boundary, with a note when it was cut.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let end = text.floor_char_boundary(max);
    format!(
        "{}\n[cut: {} of {} bytes shown]",
        &text[..end],
        end,
        text.len()
    )
}

/// The end of `text`, at most `max` bytes.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let start = text.ceil_char_boundary(text.len() - max);
    format!(
        "[cut: last {} of {} bytes]\n{}",
        text.len() - start,
        text.len(),
        &text[start..]
    )
}

fn definitions() -> Value {
    let run_id =
        json!({"type": "string", "description": "The run's id, from spawn_agent or list_agents."});
    let object = |properties: Value, required: &[&str]| {
        json!({
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        })
    };
    let tool = |name: &str, description: &str, schema: Value, read_only: bool| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": schema,
            "annotations": {"readOnlyHint": read_only, "destructiveHint": false},
        })
    };
    json!([
        tool(
            "spawn_agent",
            "Start a subagent on this project: a worker in its own git worktree that can edit files and run commands in a sandbox. Give it one self-contained task with clear done-criteria. Returns the run.",
            object(
                json!({
                    "prompt": {"type": "string", "description": "The task, at most 64 KiB."},
                    "account": {
                        "type": "object",
                        "description": "The account to run on. Omit it for the worker role's default. {\"kind\": \"subscription\", \"backend\": \"claude\"} or {\"kind\": \"key\", \"id\": \"<key account id>\"}.",
                        "properties": {
                            "kind": {"type": "string", "enum": ["subscription", "key"]},
                            "backend": {"type": "string"},
                            "id": {"type": "string"},
                        },
                        "required": ["kind"],
                    },
                }),
                &["prompt"]
            ),
            false,
        ),
        tool(
            "list_agents",
            "List this project's agent runs, oldest first, with status and diff stats.",
            object(json!({}), &[]),
            true,
        ),
        tool(
            "agent_status",
            "One run's status, its last output, and its diff stats.",
            object(json!({"runId": run_id}), &["runId"]),
            true,
        ),
        tool(
            "message_agent",
            "Send a run a message: its next turn if it is running, or a resumed session if it has ended.",
            object(
                json!({
                    "runId": run_id,
                    "text": {"type": "string", "description": "The message, at most 64 KiB."},
                }),
                &["runId", "text"]
            ),
            false,
        ),
        tool(
            "cancel_agent",
            "Stop a running run. Its changes so far are committed on its branch.",
            object(json!({"runId": run_id}), &["runId"]),
            false,
        ),
        tool(
            "agent_diff",
            "A run's latest commit against the commit its worktree started from, as unified diffs.",
            object(json!({"runId": run_id}), &["runId"]),
            true,
        ),
        tool(
            "read_context",
            "Read this project's shared context: one file's content by path, or the list of files without a path.",
            object(
                json!({
                    "path": {"type": "string", "description": "A file name such as plan.md. Omit it to list the files."},
                }),
                &[]
            ),
            true,
        ),
        tool(
            "write_context",
            "Write a file in this project's shared context, replacing it. Only .md, .markdown, and .txt names, with no folders.",
            object(
                json!({
                    "path": {"type": "string", "description": "A file name such as plan.md."},
                    "content": {"type": "string", "description": "The whole new content, at most 1 MiB."},
                }),
                &["path", "content"]
            ),
            false,
        ),
    ])
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SpawnArgs {
    prompt: String,
    #[serde(default)]
    account: Option<AccountChoice>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArgs {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RunArgs {
    run_id: RunId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct MessageArgs {
    run_id: RunId,
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadContextArgs {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteContextArgs {
    path: String,
    content: String,
}

fn parse<T: DeserializeOwned>(arguments: Value) -> Result<T, String> {
    serde_json::from_value(arguments).map_err(|error| format!("invalid arguments: {error}"))
}

fn check_text(name: &str, text: &str, max: usize) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    if text.len() > max {
        return Err(format!("{name} must be at most {max} bytes"));
    }
    Ok(())
}

async fn call_tool(binding: &Binding, name: &str, arguments: Value) -> Result<String, String> {
    match name {
        "spawn_agent" => {
            let args: SpawnArgs = parse(arguments)?;
            check_text("prompt", &args.prompt, MAX_TEXT_BYTES)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            let run = plxd
                .call::<AgentStart>(AgentStartParams {
                    run_id: RunId::generate(),
                    project: binding.project,
                    prompt: args.prompt,
                    policy: AgentPolicy::WorkspaceWrite,
                    account: args.account,
                    coordinator_thread: Some(binding.thread),
                    model: None,
                    effort: None,
                    permission: None,
                    context_window: None,
                    fast: None,
                    images: Vec::new(),
                    // plxd gives the run its coordinator's (0031).
                    approvals: false,
                    threads: Vec::new(),
                })
                .await?
                .run;
            Ok(pretty(&summary(binding, &run)))
        }
        "list_agents" => {
            let NoArgs {} = parse(arguments)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            let runs = project_runs(binding, &mut plxd).await?;
            let runs: Vec<Value> = runs.iter().map(|run| summary(binding, run)).collect();
            Ok(pretty(&json!({"runs": runs})))
        }
        "agent_status" => {
            let RunArgs { run_id } = parse(arguments)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            let run = bound_run(binding, &mut plxd, run_id).await?;
            let last_output = last_output(&mut plxd, run_id).await?;
            Ok(pretty(&json!({
                "run": summary(binding, &run),
                "lastOutput": last_output.map(|text| tail(&text, LAST_OUTPUT_BYTES)),
            })))
        }
        "message_agent" => {
            let MessageArgs { run_id, text } = parse(arguments)?;
            check_text("text", &text, MAX_TEXT_BYTES)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            bound_run(binding, &mut plxd, run_id).await?;
            let run = plxd
                .call::<AgentSend>(AgentSendParams {
                    run_id,
                    turn_id: TurnId::generate(),
                    text,
                    model: None,
                    effort: None,
                    permission: None,
                    context_window: None,
                    fast: None,
                    account: None,
                    images: Vec::new(),
                    threads: Vec::new(),
                    from: None,
                })
                .await?
                .run;
            Ok(pretty(&summary(binding, &run)))
        }
        "cancel_agent" => {
            let RunArgs { run_id } = parse(arguments)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            bound_run(binding, &mut plxd, run_id).await?;
            let run = plxd
                .call::<AgentCancel>(AgentCancelParams { run_id, from: None })
                .await?
                .run;
            Ok(pretty(&summary(binding, &run)))
        }
        "agent_diff" => {
            let RunArgs { run_id } = parse(arguments)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            bound_run(binding, &mut plxd, run_id).await?;
            let diff = plxd.call::<AgentDiff>(AgentDiffParams { run_id }).await?;
            Ok(render_diff(&diff))
        }
        _ => context_tool(binding, name, arguments).await,
    }
}

async fn context_tool(binding: &Binding, name: &str, arguments: Value) -> Result<String, String> {
    match name {
        "read_context" => {
            let ReadContextArgs { path } = parse(arguments)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            let project = binding.project;
            match path {
                None => {
                    let files = plxd
                        .call::<ContextList>(ContextListParams { project })
                        .await?
                        .files;
                    Ok(pretty(&json!({"files": files})))
                }
                Some(path) => {
                    check_text("path", &path, MAX_PATH_BYTES)?;
                    let read = plxd
                        .call::<ContextRead>(ContextReadParams { project, path })
                        .await?;
                    Ok(read.content)
                }
            }
        }
        "write_context" => {
            let WriteContextArgs { path, content } = parse(arguments)?;
            check_text("path", &path, MAX_PATH_BYTES)?;
            if content.len() > MAX_CONTEXT_BYTES {
                return Err(format!("content must be at most {MAX_CONTEXT_BYTES} bytes"));
            }
            let mut plxd = Plxd::open(&binding.socket).await?;
            let file = plxd
                .call::<ContextWrite>(ContextWriteParams {
                    id: ContextWriteId::generate(),
                    project: binding.project,
                    path,
                    content,
                    writer: Some("coordinator".to_owned()),
                })
                .await?
                .file;
            Ok(pretty(&file))
        }
        other => Err(format!("no tool is named {other:?}")),
    }
}

fn pretty(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|error| error.to_string())
}

/// What the model sees of a run.
fn summary(binding: &Binding, run: &AgentRun) -> Value {
    json!({
        "runId": run.id,
        "status": run.status,
        "prompt": clip(&run.prompt, PROMPT_PREVIEW_BYTES),
        "account": run.account_id,
        "branch": run.branch,
        "error": run.error,
        "diff": run.diff,
        "startedByThisCoordinator": run.coordinator_thread == Some(binding.thread),
        "createdAt": run.created_at,
        "updatedAt": run.updated_at,
    })
}

/// The bound project's runs, without its coordinator (0024): the model never sees or steers its
/// own run, which would message itself.
async fn project_runs(binding: &Binding, plxd: &mut Plxd) -> Result<Vec<AgentRun>, String> {
    let mut runs = plxd
        .call::<AgentList>(AgentListParams {
            project: Some(binding.project),
        })
        .await?
        .runs;
    runs.retain(|run| run.policy != AgentPolicy::NoWrite);
    Ok(runs)
}

/// Run `run_id`, if it belongs to the bound project: the binding check every tool that takes a
/// run id makes before it touches the run. Another project's run gets the same answer as one
/// that doesn't exist.
async fn bound_run(binding: &Binding, plxd: &mut Plxd, run_id: RunId) -> Result<AgentRun, String> {
    project_runs(binding, plxd)
        .await?
        .into_iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| format!("this project has no agent run {run_id}"))
}

/// The run's latest text: its last assistant message or turn result, or how its last CLI process
/// ended.
// ponytail: pages through the run's whole event history on every call; add a from-the-end page
// to agent/events if long runs make agent_status slow.
async fn last_output(plxd: &mut Plxd, run_id: RunId) -> Result<Option<String>, String> {
    let mut after = 0;
    let mut last = None;
    loop {
        let page = plxd
            .call::<AgentEvents>(AgentEventsParams {
                run_id,
                after,
                limit: Some(1000),
            })
            .await?;
        for logged in &page.events {
            match &logged.event {
                ParallaxEvent::AgentOutput { items, .. } => {
                    for item in items {
                        match item {
                            AgentOutputItem::Text { text, .. }
                            | AgentOutputItem::TurnFinished {
                                result: Some(text), ..
                            } => last = Some(text.clone()),
                            _ => {}
                        }
                    }
                }
                ParallaxEvent::AgentFinished { outcome, .. } => match outcome {
                    AgentOutcome::Completed { result: Some(text) } => last = Some(text.clone()),
                    AgentOutcome::Failed { message, .. } => last = Some(message.clone()),
                    _ => {}
                },
                _ => {}
            }
        }
        match page.events.last() {
            Some(logged) if page.more => after = logged.seq,
            _ => return Ok(last),
        }
    }
}

fn render_diff(diff: &AgentDiffResult) -> String {
    let short = |sha: &str| sha.chars().take(12).collect::<String>();
    let mut text = format!(
        "{}..{}: {} files changed, +{} -{}\n",
        short(&diff.base),
        short(&diff.head),
        diff.stats.files,
        diff.stats.insertions,
        diff.stats.deletions
    );
    for file in &diff.files {
        match &file.diff {
            Some(patch) => {
                text.push('\n');
                text.push_str(patch);
                if !patch.ends_with('\n') {
                    text.push('\n');
                }
                if file.diff_truncated {
                    text.push_str("[this file's diff was cut short]\n");
                }
            }
            None if file.binary => {
                let _ = write!(text, "\n{}: binary\n", file.path);
            }
            None => {
                let _ = write!(text, "\n{}: diff left out, over the size cap\n", file.path);
            }
        }
    }
    if diff.truncated {
        text.push_str("\n[more files changed than one answer lists]\n");
    }
    text
}

/// One connection to plxd: `initialize`d, then calls in order.
struct Plxd {
    framed: Framed<Stream, FrameCodec>,
    next_id: i64,
}

/// Errors from plxd are its message: the model reads them, and nothing matches on them.
impl Plxd {
    async fn open(socket: &Path) -> Result<Self, String> {
        let stream = transport::connect(socket)
            .await
            .map_err(|error| format!("could not reach plxd at {}: {error}", socket.display()))?;
        let mut plxd = Self {
            framed: Framed::new(stream, FrameCodec::new()),
            next_id: 0,
        };
        plxd.call::<Initialize>(InitializeParams {
            protocol: ProtocolRange::SUPPORTED,
            client: ClientInfo {
                name: "plxd mcp".to_owned(),
                version: crate::version().to_owned(),
                machine_id: None,
            },
            capabilities: Capabilities::default(),
        })
        .await?;
        Ok(plxd)
    }

    async fn call<M: RequestMethod>(&mut self, params: M::Params) -> Result<M::Result, String> {
        self.next_id += 1;
        let id = RequestId::Number(self.next_id);
        let lost =
            |error: &dyn std::fmt::Display| format!("the connection to plxd failed: {error}");
        self.framed
            .send(&Request::new::<M>(id.clone(), params))
            .await
            .map_err(|error| lost(&error))?;
        loop {
            let frame = self
                .framed
                .next()
                .await
                .ok_or_else(|| lost(&"plxd closed it"))?
                .map_err(|error| lost(&error))?;
            match Message::from_frame(&frame) {
                Ok(Message::Response(response)) if response.id.as_ref() == Some(&id) => {
                    return response.into_result().map_err(|error| error.message);
                }
                Ok(_) => {}
                Err(error) => return Err(lost(&error)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ALLOWED_TOOLS, SERVER, TOOLS, clip, definitions, tail};

    #[test]
    fn the_allowlist_is_exactly_the_tools_under_the_servers_name() {
        let expected: Vec<String> = TOOLS
            .iter()
            .map(|tool| format!("mcp__{SERVER}__{tool}"))
            .collect();
        assert_eq!(ALLOWED_TOOLS, expected.as_slice());
        let listed: Vec<String> = definitions()
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(listed, TOOLS);
    }

    #[test]
    fn the_coordinators_instructions_name_only_real_tools() {
        let instructions = include_str!("agents/coordinator.md");
        // Every `snake_case` span between backticks.
        for name in instructions.split('`').skip(1).step_by(2) {
            if name.contains('_') && name.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                assert!(
                    TOOLS.contains(&name),
                    "coordinator.md names `{name}`, not a tool"
                );
            }
        }
    }

    #[test]
    fn no_tool_takes_a_project_or_a_thread() {
        for tool in definitions().as_array().unwrap() {
            let schema = &tool["inputSchema"];
            assert_eq!(schema["additionalProperties"], false, "{tool}");
            let properties = schema["properties"].as_object().unwrap();
            for name in properties.keys() {
                assert!(
                    !name.to_lowercase().contains("project")
                        && !name.to_lowercase().contains("thread"),
                    "{} takes {name}",
                    tool["name"]
                );
            }
        }
    }

    #[test]
    fn long_text_is_cut_at_a_character_boundary_with_a_note() {
        assert_eq!(clip("short", 10), "short");
        let cut = clip("ééééé", 3);
        assert!(cut.starts_with("é\n[cut: 2 of 10 bytes shown]"), "{cut}");
        let end = tail("ééééé", 3);
        assert!(end.ends_with("\né"), "{end}");
    }
}
