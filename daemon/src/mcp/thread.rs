//! `plxd mcp --thread <runId>`: a normal thread's host-wide Parallax tools (0041, PLX-373).
//!
//! plxd writes `--thread` into the thread's `--mcp-config` ([`crate::backend::ThreadTools`]), so
//! the server knows its caller and no tool takes the caller's id. The tools reach every thread on
//! the host, as the user can with clicks: list, read, launch, message, wait on, interrupt, rename,
//! settle, and archive them, and link pull requests to them. A launched thread records the caller
//! as its `parent`, and a message or interrupt names the caller in the target's transcript
//! (`agent/send`'s and `agent/cancel`'s `from`). Framing, connections, errors, and size caps are
//! 0019's, from [`super`].

use std::path::{Path, PathBuf};
use std::time::Duration;

use parallax_protocol::methods::{
    AgentCancel, AgentEvents, AgentList, AgentSend, PrLink, PrUnlink, RepoAdd, ThreadArchive,
    ThreadList, ThreadSearch, ThreadStart, ThreadUpdate,
};
use parallax_protocol::{
    AccountChoice, AccountId, AgentCancelParams, AgentEffort, AgentEventsParams, AgentListParams,
    AgentOutcome, AgentOutputItem, AgentPermission, AgentRun, AgentSendParams, AgentStatus,
    AgentToolStatus, ParallaxEvent, PrViewParams, Repo, RepoAddParams, RepoId, RunId, Thread,
    ThreadArchiveParams, ThreadListParams, ThreadListResult, ThreadSearchParams, ThreadStartParams,
    ThreadUpdateParams, TurnId,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{Instant, sleep};

use super::{MAX_TEXT_BYTES, Plxd, Tools, check_text, clip, last_output, parse, pretty, tail};

/// Every tool the server offers.
pub const TOOLS: &[&str] = &[
    "thread_list",
    "thread_read",
    "thread_search",
    "thread_launch",
    "thread_send",
    "thread_wait",
    "thread_interrupt",
    "thread_update",
    "pr_link",
    "pr_unlink",
];

/// [`TOOLS`] as Claude Code names them, `mcp__<server>__<tool>`: a thread's `--allowedTools`, so
/// they run without asking in every permission mode. Claude Code's todo tools follow them there.
pub const ALLOWED_TOOLS: &[&str] = &[
    "mcp__plxd__thread_list",
    "mcp__plxd__thread_read",
    "mcp__plxd__thread_search",
    "mcp__plxd__thread_launch",
    "mcp__plxd__thread_send",
    "mcp__plxd__thread_wait",
    "mcp__plxd__thread_interrupt",
    "mcp__plxd__thread_update",
    "mcp__plxd__pr_link",
    "mcp__plxd__pr_unlink",
];

/// About how much transcript one `thread_read` page carries, in bytes. A page ends at an event,
/// so it can run over by one event's text, which [`ITEM_BYTES`] caps.
const PAGE_BYTES: usize = 64 * 1024;

/// The most of one message or tool output a page shows, in bytes.
const ITEM_BYTES: usize = 16 * 1024;

/// How much of a tool call's input or result a page shows, in bytes.
const TOOL_BYTES: usize = 300;

/// How much of a thread's prompt `thread_list` and the other summaries show.
const PROMPT_PREVIEW_BYTES: usize = 500;

/// How much of a thread's last output `thread_wait` shows: its end.
const LAST_OUTPUT_BYTES: usize = 8 * 1024;

/// How long `thread_wait` waits without a `timeoutSeconds`, and the most it takes.
const DEFAULT_WAIT: Duration = Duration::from_mins(5);
const MAX_WAIT: Duration = Duration::from_mins(30);

/// How often `thread_wait` checks the thread.
const POLL: Duration = Duration::from_millis(500);

/// What one server is bound to.
#[derive(Clone, Debug)]
pub struct Binding {
    /// plxd's socket.
    pub socket: PathBuf,
    /// The calling thread's run.
    pub run: RunId,
}

/// Checks that the bound run exists, then serves MCP on `input` and `output` until `input` ends.
///
/// # Errors
///
/// When plxd can't be reached or doesn't know the run, when a line is longer than
/// [`super::MAX_MESSAGE_BYTES`], or when reading or writing fails.
pub async fn run(
    binding: &Binding,
    input: impl AsyncRead + Unpin,
    output: impl AsyncWrite + Unpin,
) -> Result<(), String> {
    let mut plxd = Plxd::open(&binding.socket).await?;
    find_run(&mut plxd, binding.run).await?;
    drop(plxd);
    super::serve(binding, input, output).await
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

#[expect(
    clippy::too_many_lines,
    reason = "one schema per tool, read side by side"
)]
fn definitions() -> Value {
    let run_id = |what: &str| json!({"type": "string", "description": what});
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
    let target = run_id("The thread's run id, from thread_list or thread_launch.");
    let threads = json!({
        "type": "array",
        "items": {"type": "string"},
        "maxItems": 8,
        "description": "Run ids of threads whose summaries to attach to the message as context, at most 8.",
    });
    let mine = run_id("The thread's run id, from thread_list. Omit it for your own thread.");
    json!([
        tool(
            "thread_list",
            "List the threads on this host, newest first: each one's run id, title, status, backend, model, mode, repository, branch, parent, and whether it is settled. Yours has \"you\": true.",
            object(
                json!({
                    "includeArchived": {"type": "boolean", "description": "Also list archived threads. Default false."},
                }),
                &[]
            ),
            true,
        ),
        tool(
            "thread_read",
            "Read a thread's transcript, oldest first, about 64 KiB a page: its messages and who sent them, the agent's replies, tool calls, and how its runs ended. A longer one ends with the `after` to pass for the next page.",
            object(
                json!({
                    "runId": target,
                    "after": {"type": "integer", "minimum": 0, "description": "Read the events after this one, from the last page's note. Default 0, the start."},
                }),
                &["runId"]
            ),
            true,
        ),
        tool(
            "thread_search",
            "Find threads whose messages or replies contain some text, not counting tool calls, the one with the newest message first. Case-insensitive for ASCII letters.",
            object(
                json!({
                    "query": {"type": "string", "description": "The text to find."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100, "description": "The most threads to return. Default 20."},
                }),
                &["query"]
            ),
            true,
        ),
        tool(
            "thread_launch",
            "Start a new thread as your child, on any backend, model, and mode, and return it at once. It runs on its own: use thread_wait to wait for it and thread_read to read what it did. Without a workspace it works in a new worktree of your repository, or with no repository if you have none.",
            object(
                json!({
                    "prompt": {"type": "string", "description": "The first message, at most 64 KiB. Make it self-contained."},
                    "threads": threads,
                    "title": {"type": "string", "description": "Its title in the sidebar, at most 256 bytes."},
                    "backend": {"type": "string", "description": "The CLI to run it on with the user's own login, such as claude, codex, or cursor. Omit it for the user's default."},
                    "account": {"type": "string", "description": "A key account's id to run it on instead of a backend's login."},
                    "model": {"type": "string", "description": "The model, such as claude-sonnet-4-6. Omit it for the CLI's default."},
                    "effort": {"type": "string", "enum": ["low", "medium", "high", "xhigh", "max"]},
                    "mode": {"type": "string", "enum": ["auto", "manual", "edit", "plan", "bypass"], "description": "Its permission mode. Omit it for yours, on the same backend."},
                    "workspace": {"type": "string", "enum": ["worktree", "checkout", "none"], "description": "worktree: a new git worktree of the repository; checkout: the repository's own checkout, whose changes stay uncommitted there; none: no repository."},
                    "repo": {"type": "string", "description": "The repository, by absolute path or thread_list's repo id. Default: yours."},
                    "base": {"type": "string", "description": "With worktree, the ref it starts from, such as origin/develop. Default HEAD."},
                    "branch": {"type": "string", "description": "With checkout, the branch to switch the checkout to first."},
                }),
                &["prompt"]
            ),
            false,
        ),
        tool(
            "thread_send",
            "Send a thread a message, marked in its transcript as from you. A running thread gets it after its current turn; a stopped one resumes with it.",
            object(
                json!({
                    "runId": target,
                    "text": {"type": "string", "description": "The message, at most 64 KiB."},
                    "threads": threads,
                }),
                &["runId", "text"]
            ),
            false,
        ),
        tool(
            "thread_wait",
            "Wait until a thread is idle, its turn finished or stopped, or until a timeout, then return its status and the end of its last output.",
            object(
                json!({
                    "runId": target,
                    "timeoutSeconds": {"type": "integer", "minimum": 1, "maximum": MAX_WAIT.as_secs(), "description": "How long to wait. Default 300."},
                }),
                &["runId"]
            ),
            true,
        ),
        tool(
            "thread_interrupt",
            "Stop a thread's running turn, marked in its transcript as stopped by you. What it changed so far stays.",
            object(json!({"runId": target}), &["runId"]),
            false,
        ),
        tool(
            "thread_update",
            "Rename a thread, mark it settled (nothing left to do) or not, or archive it or bring it back.",
            object(
                json!({
                    "runId": mine,
                    "title": {"type": "string", "description": "Its new title, at most 256 bytes. Empty clears it."},
                    "settled": {"type": "boolean"},
                    "archived": {"type": "boolean"},
                }),
                &[]
            ),
            false,
        ),
        tool(
            "pr_link",
            "Link a GitHub pull request to a thread, so the app shows it there.",
            object(
                json!({
                    "url": {"type": "string", "description": "The pull request's URL, such as https://github.com/owner/repo/pull/1."},
                    "runId": mine,
                }),
                &["url"]
            ),
            false,
        ),
        tool(
            "pr_unlink",
            "Remove a pull request from a thread's links.",
            object(
                json!({
                    "url": {"type": "string", "description": "The pull request's URL, as the thread lists it."},
                    "runId": mine,
                }),
                &["url"]
            ),
            false,
        ),
    ])
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ListArgs {
    #[serde(default)]
    include_archived: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReadArgs {
    run_id: RunId,
    #[serde(default)]
    after: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SearchArgs {
    query: String,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Workspace {
    Worktree,
    Checkout,
    None,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LaunchArgs {
    prompt: String,
    #[serde(default)]
    threads: Vec<RunId>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    account: Option<AccountId>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    effort: Option<AgentEffort>,
    #[serde(default)]
    mode: Option<AgentPermission>,
    #[serde(default)]
    workspace: Option<Workspace>,
    #[serde(default)]
    repo: Option<String>,
    #[serde(default)]
    base: Option<String>,
    #[serde(default)]
    branch: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SendArgs {
    run_id: RunId,
    text: String,
    #[serde(default)]
    threads: Vec<RunId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct WaitArgs {
    run_id: RunId,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TargetArgs {
    run_id: RunId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UpdateArgs {
    #[serde(default)]
    run_id: Option<RunId>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    settled: Option<bool>,
    #[serde(default)]
    archived: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PrArgs {
    url: String,
    #[serde(default)]
    run_id: Option<RunId>,
}

async fn call_tool(binding: &Binding, name: &str, arguments: Value) -> Result<String, String> {
    let caller = binding.run;
    match name {
        "thread_list" => {
            let ListArgs { include_archived } = parse(arguments)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            let mut threads = plxd.call::<ThreadList>(ThreadListParams {}).await?.threads;
            threads.reverse();
            threads.retain(|thread| include_archived || !thread.archived);
            described(&mut plxd, &threads, caller).await
        }
        "thread_read" => {
            let ReadArgs { run_id, after } = parse(arguments)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            find_run(&mut plxd, run_id).await?;
            read(&mut plxd, run_id, after).await
        }
        "thread_search" => {
            let SearchArgs { query, limit } = parse(arguments)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            let found = plxd
                .call::<ThreadSearch>(ThreadSearchParams { query, limit })
                .await?;
            described(&mut plxd, &found.threads, caller).await
        }
        "thread_launch" => launch(binding, parse(arguments)?).await,
        "thread_send" => {
            let SendArgs {
                run_id,
                text,
                threads,
            } = parse(arguments)?;
            not_yourself(caller, run_id, "send a message to")?;
            check_text("text", &text, MAX_TEXT_BYTES)?;
            let mut plxd = Plxd::open(&binding.socket).await?;
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
                    threads,
                    from: Some(caller),
                })
                .await?
                .run;
            Ok(pretty(&describe(&run, None, &[], caller)))
        }
        "thread_wait" => {
            let WaitArgs {
                run_id,
                timeout_seconds,
            } = parse(arguments)?;
            not_yourself(caller, run_id, "wait on")?;
            let timeout = timeout_seconds.map_or(DEFAULT_WAIT, Duration::from_secs);
            wait(&binding.socket, run_id, timeout.min(MAX_WAIT), caller).await
        }
        "thread_interrupt" => {
            let TargetArgs { run_id } = parse(arguments)?;
            not_yourself(caller, run_id, "interrupt")?;
            let mut plxd = Plxd::open(&binding.socket).await?;
            let run = plxd
                .call::<AgentCancel>(AgentCancelParams {
                    run_id,
                    from: Some(caller),
                })
                .await?
                .run;
            Ok(pretty(&describe(&run, None, &[], caller)))
        }
        "thread_update" => update(binding, parse(arguments)?).await,
        "pr_link" | "pr_unlink" => {
            let PrArgs { url, run_id } = parse(arguments)?;
            let params = PrViewParams {
                run_id: run_id.unwrap_or(caller),
                url,
            };
            let mut plxd = Plxd::open(&binding.socket).await?;
            let run = if name == "pr_link" {
                plxd.call::<PrLink>(params).await?.run
            } else {
                plxd.call::<PrUnlink>(params).await?.run
            };
            Ok(pretty(
                &json!({"runId": run.id, "pullRequests": run.pull_requests}),
            ))
        }
        other => Err(format!("no tool is named {other:?}")),
    }
}

/// `{"threads": [...]}`, each of `threads` with its run, in their order.
async fn described(plxd: &mut Plxd, threads: &[Thread], caller: RunId) -> Result<String, String> {
    let (listed, runs) = host(plxd).await?;
    let threads: Vec<Value> = threads
        .iter()
        .filter_map(|thread| {
            let run = runs.iter().find(|run| run.id == thread.id)?;
            Some(describe(run, Some(thread), &listed.repos, caller))
        })
        .collect();
    Ok(pretty(&json!({"threads": threads})))
}

/// Refuses a tool that would act on the caller itself: a thread can't wait on, message, or stop
/// its own turn from inside it.
fn not_yourself(caller: RunId, target: RunId, what: &str) -> Result<(), String> {
    if caller == target {
        return Err(format!("a thread can't {what} itself"));
    }
    Ok(())
}

/// Every thread and repo entry, and every run on the host.
async fn host(plxd: &mut Plxd) -> Result<(ThreadListResult, Vec<AgentRun>), String> {
    let listed = plxd.call::<ThreadList>(ThreadListParams {}).await?;
    let runs = plxd
        .call::<AgentList>(AgentListParams { project: None })
        .await?
        .runs;
    Ok((listed, runs))
}

/// The run `run_id`, a thread's or any other on the host.
async fn find_run(plxd: &mut Plxd, run_id: RunId) -> Result<AgentRun, String> {
    plxd.call::<AgentList>(AgentListParams { project: None })
        .await?
        .runs
        .into_iter()
        .find(|run| run.id == run_id)
        .ok_or_else(|| format!("no thread has run id {run_id}"))
}

/// What the model sees of a run, and of its thread when it is one.
fn describe(run: &AgentRun, thread: Option<&Thread>, repos: &[Repo], caller: RunId) -> Value {
    let repo = thread
        .and_then(|thread| repos.iter().find(|repo| repo.id == thread.repo))
        .filter(|repo| !repo.scratch);
    let mut value = json!({
        "runId": run.id,
        "you": run.id == caller,
        "status": run.status,
        "backend": run.backend,
        "model": run.model,
        "mode": run.permission,
        "prompt": clip(&run.prompt, PROMPT_PREVIEW_BYTES),
        "branch": run.branch,
        "worktreePath": run.worktree_path,
        "checkout": run.checkout,
        "error": run.error,
        "pullRequests": run.pull_requests,
        "createdAt": run.created_at,
        "updatedAt": run.updated_at,
    });
    if let Some(thread) = thread {
        value["title"] = json!(thread.title);
        value["repo"] = json!(repo.map(|repo| json!({"id": repo.id, "path": repo.path})));
        value["parent"] = json!(thread.parent);
        value["settled"] = json!(thread.settled);
        value["archived"] = json!(thread.archived);
    }
    value
}

/// One page of `run_id`'s transcript after event `after`, rendered for the model.
async fn read(plxd: &mut Plxd, run_id: RunId, after: u64) -> Result<String, String> {
    let mut page = String::new();
    let mut render = Render::default();
    let mut cursor = after;
    loop {
        let events = plxd
            .call::<AgentEvents>(AgentEventsParams {
                run_id,
                after: cursor,
                limit: Some(1000),
            })
            .await?;
        for logged in &events.events {
            let text = render.event(&logged.event);
            if !page.is_empty() && page.len() + text.len() > PAGE_BYTES {
                return Ok(format!(
                    "{page}\n[more: call thread_read with after={cursor} for the next page]\n"
                ));
            }
            page.push_str(&text);
            cursor = logged.seq;
        }
        if !events.more || events.events.is_empty() {
            if page.is_empty() {
                page.push_str("[nothing after this point yet]\n");
            }
            return Ok(page);
        }
    }
}

/// Renders a run's logged events as a transcript, one at a time.
#[derive(Default)]
struct Render {
    /// The agent's last message, which a turn's result often repeats.
    last_reply: String,
}

impl Render {
    fn event(&mut self, event: &ParallaxEvent) -> String {
        match event {
            ParallaxEvent::AgentStarted { run: Some(run), .. } => {
                format!(
                    "First message:\n{}\n\n",
                    clip(run.prompt.trim(), ITEM_BYTES)
                )
            }
            ParallaxEvent::AgentOutput { items, .. } => {
                items.iter().map(|item| self.item(item)).collect()
            }
            ParallaxEvent::AgentFinished { outcome, .. } => {
                let how = match outcome {
                    AgentOutcome::Completed { .. } => "completed".to_owned(),
                    AgentOutcome::Cancelled => "stopped".to_owned(),
                    AgentOutcome::Failed { message, .. } => {
                        format!("failed: {}", clip(message, TOOL_BYTES))
                    }
                    AgentOutcome::Interrupted => "interrupted by a plxd restart".to_owned(),
                    AgentOutcome::Unknown => "ended".to_owned(),
                };
                format!("[run {how}]\n\n")
            }
            _ => String::new(),
        }
    }

    fn item(&mut self, item: &AgentOutputItem) -> String {
        match item {
            AgentOutputItem::TurnStarted {
                text: Some(text),
                wake,
                from,
                ..
            } => {
                let who = match from {
                    _ if *wake => "Parallax".to_owned(),
                    Some(from) => format!("Thread {from}"),
                    None => "User".to_owned(),
                };
                format!("{who}:\n{}\n\n", clip(text.trim(), ITEM_BYTES))
            }
            AgentOutputItem::Text { text, .. } if !text.trim().is_empty() => {
                text.trim().clone_into(&mut self.last_reply);
                format!("Agent:\n{}\n\n", clip(&self.last_reply, ITEM_BYTES))
            }
            AgentOutputItem::TurnFinished {
                result: Some(result),
                ..
            } if !result.trim().is_empty() && result.trim() != self.last_reply => {
                result.trim().clone_into(&mut self.last_reply);
                format!("Agent:\n{}\n\n", clip(&self.last_reply, ITEM_BYTES))
            }
            AgentOutputItem::ToolCall { name, input, .. } => {
                format!("[tool {name}: {}]\n", clip(&input.to_string(), TOOL_BYTES))
            }
            AgentOutputItem::ToolResult { status, output, .. } => {
                let status = match status {
                    AgentToolStatus::Ok => "ok",
                    AgentToolStatus::Error => "error",
                    AgentToolStatus::Denied => "denied",
                    AgentToolStatus::Unknown => "ended",
                };
                match output {
                    Some(output) => format!("[result {status}: {}]\n", clip(output, TOOL_BYTES)),
                    None => format!("[result {status}]\n"),
                }
            }
            AgentOutputItem::Interrupted { from } => format!("[stopped by thread {from}]\n"),
            AgentOutputItem::Notice { detail } | AgentOutputItem::Warning { detail } => {
                format!("[{}]\n", clip(detail, TOOL_BYTES))
            }
            AgentOutputItem::FollowUpDropped { .. } => {
                "[a message never reached the agent]\n".to_owned()
            }
            AgentOutputItem::ApprovalRequested { tool_name, .. } => {
                format!("[asks the user to allow {tool_name}]\n")
            }
            _ => String::new(),
        }
    }
}

/// `thread_launch`: a new thread whose parent is the caller.
async fn launch(binding: &Binding, args: LaunchArgs) -> Result<String, String> {
    let LaunchArgs {
        prompt,
        threads,
        title,
        backend,
        account,
        model,
        effort,
        mode,
        workspace,
        repo,
        base,
        branch,
    } = args;
    check_text("prompt", &prompt, MAX_TEXT_BYTES)?;
    let account = match (backend, account) {
        (Some(_), Some(_)) => return Err("give a backend or an account, not both".to_owned()),
        (Some(backend), None) => Some(AccountChoice::Subscription { backend }),
        (None, Some(id)) => Some(AccountChoice::Key { id }),
        (None, None) => None,
    };
    let mut plxd = Plxd::open(&binding.socket).await?;
    let (listed, runs) = host(&mut plxd).await?;
    let caller = runs
        .iter()
        .find(|run| run.id == binding.run)
        .ok_or("your thread is no longer on this host")?;
    // The caller's own repository, unless it has none.
    let own_repo = listed
        .threads
        .iter()
        .find(|thread| thread.id == binding.run)
        .and_then(|thread| listed.repos.iter().find(|repo| repo.id == thread.repo))
        .filter(|repo| !repo.scratch)
        .map(|repo| repo.id);
    let repo = match repo {
        Some(repo) => Some(resolve_repo(&mut plxd, &listed.repos, &repo).await?),
        None => own_repo,
    };
    let workspace = workspace.unwrap_or(if repo.is_some() {
        Workspace::Worktree
    } else {
        Workspace::None
    });
    let (repo, checkout) = match workspace {
        Workspace::None if base.is_some() || branch.is_some() => {
            return Err("base and branch need a repository's workspace".to_owned());
        }
        Workspace::None => (None, false),
        Workspace::Worktree if branch.is_some() => {
            return Err("branch goes with workspace checkout; use base for a worktree".to_owned());
        }
        Workspace::Checkout if base.is_some() => {
            return Err("base goes with workspace worktree; use branch for a checkout".to_owned());
        }
        Workspace::Worktree | Workspace::Checkout => {
            let repo = repo.ok_or("you have no repository; name one in repo")?;
            (Some(repo), matches!(workspace, Workspace::Checkout))
        }
    };
    // The caller's mode, as a coordinator's subagents get its (0027), when the child runs on the
    // same backend, which maps it.
    let same_backend = match &account {
        None => true,
        Some(AccountChoice::Subscription { backend }) => caller.backend == *backend,
        Some(_) => false,
    };
    let permission = mode.or_else(|| same_backend.then_some(caller.permission).flatten());
    check_mode(caller.permission, permission)?;
    let started = plxd
        .call::<ThreadStart>(ThreadStartParams {
            run_id: RunId::generate(),
            repo,
            parent: Some(binding.run),
            title,
            prompt,
            account,
            model,
            effort,
            permission,
            context_window: None,
            fast: None,
            branch_slug: None,
            images: Vec::new(),
            threads,
            // As a thread the user starts: full Claude Code, asking the app (0034).
            approvals: true,
            checkout,
            base,
            checkout_ref: branch,
        })
        .await?;
    let (listed, _) = host(&mut plxd).await?;
    Ok(pretty(&describe(
        &started.run,
        Some(&started.thread),
        &listed.repos,
        binding.run,
    )))
}

/// Refuses a child `mode` that needs less approval than the caller's `theirs` (0041). No mode
/// means Edit.
fn check_mode(
    theirs: Option<AgentPermission>,
    mode: Option<AgentPermission>,
) -> Result<(), String> {
    let theirs = theirs.unwrap_or(AgentPermission::Edit);
    let child = mode.unwrap_or(AgentPermission::Edit);
    if reach(child).is_some_and(|child| reach(theirs) >= Some(child)) {
        return Ok(());
    }
    let name = |mode| crate::agents::convert::option_name(mode).unwrap_or_default();
    Err(format!(
        "you run in {} mode, so a thread you launch can't run in {}, which needs less approval",
        name(theirs),
        name(child)
    ))
}

/// How much `mode` lets a run do without asking, least first, or `None` for a mode this plxd
/// doesn't know.
fn reach(mode: AgentPermission) -> Option<u8> {
    match mode {
        AgentPermission::Plan => Some(0),
        AgentPermission::Manual => Some(1),
        AgentPermission::Edit => Some(2),
        AgentPermission::Auto => Some(3),
        AgentPermission::Bypass => Some(4),
        AgentPermission::Unknown => None,
    }
}

/// The repo entry `repo` names, by id or by path, registering a repository on the host that has
/// none yet, as adding it in the app does.
async fn resolve_repo(plxd: &mut Plxd, repos: &[Repo], repo: &str) -> Result<RepoId, String> {
    if let Some(found) = repos
        .iter()
        .find(|entry| !entry.scratch && entry.id.to_string() == repo)
    {
        return Ok(found.id);
    }
    let path = Path::new(repo);
    if !path.is_absolute() {
        return Err(format!(
            "repo {repo:?} is neither a repo id from thread_list nor an absolute path"
        ));
    }
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    if let Some(found) = repos
        .iter()
        .find(|entry| !entry.scratch && Path::new(&entry.path) == canonical)
    {
        return Ok(found.id);
    }
    let added = plxd
        .call::<RepoAdd>(RepoAddParams {
            id: RepoId::generate(),
            path: canonical.to_string_lossy().into_owned(),
        })
        .await?;
    Ok(added.repo.id)
}

/// `thread_wait`: checks `run_id` every [`POLL`] until it is idle or `timeout` passes, each time
/// on a new connection, so a plxd restart meanwhile doesn't end the wait.
async fn wait(
    socket: &Path,
    run_id: RunId,
    timeout: Duration,
    caller: RunId,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match Plxd::open(socket).await {
            Ok(mut plxd) => {
                let run = find_run(&mut plxd, run_id).await?;
                let idle = !matches!(run.status, AgentStatus::Starting | AgentStatus::Running);
                if idle || Instant::now() >= deadline {
                    let output = last_output(&mut plxd, run_id).await?;
                    return Ok(pretty(&json!({
                        "idle": idle,
                        "timedOut": !idle,
                        "thread": describe(&run, None, &[], caller),
                        "lastOutput": output.map(|text| tail(&text, LAST_OUTPUT_BYTES)),
                    })));
                }
            }
            Err(error) if Instant::now() >= deadline => return Err(error),
            // plxd may be restarting: try again until the deadline.
            Err(_) => {}
        }
        sleep(POLL).await;
    }
}

/// `thread_update`: `thread/update` for a title or settled, then `thread/archive`.
async fn update(binding: &Binding, args: UpdateArgs) -> Result<String, String> {
    let UpdateArgs {
        run_id,
        title,
        settled,
        archived,
    } = args;
    if title.is_none() && settled.is_none() && archived.is_none() {
        return Err("give a title, settled, or archived".to_owned());
    }
    let run_id = run_id.unwrap_or(binding.run);
    let mut plxd = Plxd::open(&binding.socket).await?;
    let mut thread = None;
    if title.is_some() || settled.is_some() {
        let updated = plxd
            .call::<ThreadUpdate>(ThreadUpdateParams {
                run_id,
                seen: false,
                snoozed_until: None,
                title,
                settled,
            })
            .await?;
        thread = Some(updated.thread);
    }
    if let Some(archived) = archived {
        let updated = plxd
            .call::<ThreadArchive>(ThreadArchiveParams { run_id, archived })
            .await?;
        thread = Some(updated.thread);
    }
    Ok(pretty(&thread))
}

#[cfg(test)]
mod tests {
    use super::{ALLOWED_TOOLS, TOOLS, definitions};
    use crate::mcp::SERVER;

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

    /// The caller is bound by `--thread`: no tool takes it, so only a target may be named.
    #[test]
    fn no_tool_takes_the_callers_id_or_unknown_fields() {
        for tool in definitions().as_array().unwrap() {
            let schema = &tool["inputSchema"];
            assert_eq!(schema["additionalProperties"], false, "{tool}");
            let properties = schema["properties"].as_object().unwrap();
            for name in properties.keys() {
                let name = name.to_lowercase();
                assert!(
                    !["caller", "from", "parent", "self"].contains(&name.as_str()),
                    "{} takes {name}",
                    tool["name"]
                );
            }
        }
    }
}
