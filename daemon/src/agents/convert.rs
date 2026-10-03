//! Mapping between the runner's inputs and outputs: backend events to the protocol's transcript
//! items, and store rows to the protocol's runs.

use jiff::Timestamp;
use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::{
    AgentApproveResult, AgentFailureKind, AgentMerge, AgentMergeKind, AgentOutcome,
    AgentOutputItem, AgentPolicy, AgentRun, AgentRunState, AgentStatus, AgentTodoItem,
    AgentTodoStatus, AgentToolStatus, ApprovalId, CoordinatorThreadId, DiffSummary, ProjectId,
    RunId,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tracing::error;

use crate::backend::event::{MAX_ALWAYS_ALLOW_RULE_BYTES, MAX_ALWAYS_ALLOW_RULES};
use crate::backend::{
    ApprovalRequest, Event, FailureKind, Outcome, TodoItem, TodoStatus, ToolStatus,
};
use crate::json::escaped_len;
use crate::worktree::MergeHow;

/// The longest tool output an `agent.output` item carries, in bytes. The rest is cut, since the
/// full output stays in the CLI's own session and 0007 caps a frame at 8 MiB.
pub(super) const MAX_TOOL_OUTPUT_BYTES: usize = 32 * 1024;

/// The largest tool input an `agent.output` item carries as JSON, in bytes. A larger one, such as
/// a `Write` of a big file, becomes `{"truncated": true, "bytes": n}`.
pub(super) const MAX_TOOL_INPUT_BYTES: usize = 32 * 1024;

/// The largest tool input an `approvalRequested` item carries as JSON, in bytes (RYA-222): more
/// than a tool call's, since the user has to see what they allow, such as a long plan.
pub(super) const MAX_APPROVAL_INPUT_BYTES: usize = 256 * 1024;

/// The longest free text field of any other `agent.output` item — `Text`, `TextDelta`,
/// `Reasoning`, `Notice.detail`, `Warning.detail`, `TurnFinished.result`, `TurnStarted.text` — in
/// bytes, at the same cap as a tool's output (#190 N8). Without this, one item near 0007's 8 MiB
/// frame would close every subscriber and then be replayed again on every reconnect.
pub(super) const MAX_TEXT_ITEM_BYTES: usize = 32 * 1024;

/// The longest a short identifier gets to be, in bytes: `ToolCall`'s `call_id` and `name`,
/// `ToolResult`'s `call_id`, `SessionStarted`'s `session_id` and `model`, and every `message_id`.
/// These are meant to be short, vendor-assigned tokens; this is defense in depth against a vendor
/// bug or a hostile CLI reporting one that isn't (#190 review).
pub(super) const MAX_ID_BYTES: usize = 1024;

/// The longest one `TodoList` item's `text` gets to be, in bytes.
pub(super) const MAX_TODO_TEXT_BYTES: usize = 4 * 1024;

/// The most a `TodoList`'s items add up to, counted as JSON-escaped bytes rather than raw ones
/// (#190 review): a raw count could undercount a list whose text needs a lot of escaping, letting
/// the list's real JSON size run well past what the raw count suggested. Items past this are
/// dropped, not only their text, since even a short text per item adds up at an unbounded count.
pub(super) const MAX_TODO_LIST_BYTES: usize = 64 * 1024;

pub(super) const STARTING: &str = "starting";
pub(super) const RUNNING: &str = "running";
pub(super) const COMPLETED: &str = "completed";
pub(super) const FAILED: &str = "failed";
pub(super) const CANCELLED: &str = "cancelled";
pub(super) const INTERRUPTED: &str = "interrupted";
pub(super) const WAITING: &str = "waiting";
pub(super) const ACCEPTED: &str = "accepted";

/// The store's text for the only policy `agent/start` takes.
pub(super) const WORKSPACE_WRITE: &str = "workspaceWrite";

/// The store's text for a project coordinator's policy (0024).
pub(crate) const NO_WRITE: &str = "noWrite";

fn status(text: &str) -> AgentStatus {
    match text {
        STARTING => AgentStatus::Starting,
        RUNNING => AgentStatus::Running,
        COMPLETED => AgentStatus::Completed,
        FAILED => AgentStatus::Failed,
        CANCELLED => AgentStatus::Cancelled,
        INTERRUPTED => AgentStatus::Interrupted,
        WAITING => AgentStatus::Waiting,
        ACCEPTED => AgentStatus::Accepted,
        _ => AgentStatus::Unknown,
    }
}

/// A store row and its worktree as the protocol's run.
///
/// plxd writes only version 7 ids, so a row with another kind of id was written by something
/// else, and the request fails rather than hide the row.
pub(crate) fn agent_run(
    row: &parallax_store::Run,
    worktree: Option<&parallax_store::Worktree>,
) -> Result<AgentRun, ErrorObject> {
    let corrupt = |what: &str| {
        error!(run = %row.id, what, "a stored run has an invalid id");
        ErrorObject::internal_error(format!("the stored run {} has an invalid {what}", row.id))
    };
    let id = RunId::try_from(row.id).map_err(|_| corrupt("id"))?;
    let project = ProjectId::try_from(row.fields.project_id).map_err(|_| corrupt("project id"))?;
    let state = &row.state;
    Ok(AgentRun {
        id,
        project,
        prompt: row.fields.prompt.clone(),
        policy: match row.fields.policy.as_str() {
            WORKSPACE_WRITE => AgentPolicy::WorkspaceWrite,
            NO_WRITE => AgentPolicy::NoWrite,
            _ => AgentPolicy::Unknown,
        },
        status: status(&state.status),
        backend: row.fields.backend.clone(),
        account_id: state.account_id.clone(),
        branch: worktree.map(|worktree| worktree.branch.clone()),
        worktree_path: worktree.map(|worktree| worktree.path.clone()),
        base_dirty: worktree.is_some_and(|worktree| worktree.base_dirty),
        session_id: state.session_id.clone(),
        error: state.error.clone(),
        diff: diff(state),
        coordinator_thread: row
            .fields
            .coordinator_thread
            .map(CoordinatorThreadId::try_from)
            .transpose()
            .map_err(|_| corrupt("coordinator thread id"))?,
        model: row.fields.model.clone(),
        effort: row.fields.effort.as_deref().and_then(option_value),
        permission: row.fields.permission.as_deref().and_then(option_value),
        context_window: row.fields.context_window,
        fast: row.fields.fast,
        approvals: row.fields.approvals,
        checkout: row.fields.checkout,
        pull_requests: state.pull_requests.clone(),
        resume_at: state.resume_at,
        auto_resume: state.auto_resume,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// An effort's or a permission's protocol name, such as `high`, as the runs table stores it.
pub(crate) fn option_name(value: impl Serialize) -> Option<String> {
    serde_json::to_value(value)
        .ok()?
        .as_str()
        .map(str::to_owned)
}

/// A stored effort or permission, back from its protocol name: `Unknown` for a name this version
/// doesn't know.
pub(crate) fn option_value<T: DeserializeOwned>(name: &str) -> Option<T> {
    serde_json::from_value(Value::String(name.to_owned())).ok()
}

/// The part of a stored run that `agent.updated` reports.
pub(super) fn run_state(row: &parallax_store::Run) -> AgentRunState {
    let state = &row.state;
    AgentRunState {
        status: status(&state.status),
        account_id: state.account_id.clone(),
        backend: Some(row.fields.backend.clone()),
        session_id: state.session_id.clone(),
        error: state.error.clone(),
        diff: diff(state),
        model: row.fields.model.clone(),
        effort: row.fields.effort.as_deref().and_then(option_value),
        permission: row.fields.permission.as_deref().and_then(option_value),
        context_window: row.fields.context_window,
        fast: row.fields.fast,
        pull_requests: state.pull_requests.clone(),
        resume_at: state.resume_at,
        auto_resume: state.auto_resume,
        updated_at: row.updated_at,
    }
}

/// The store's text for how `agent/accept` merged a run.
pub(super) fn merge_how_text(how: MergeHow) -> &'static str {
    match how {
        MergeHow::FastForward => "fastForward",
        MergeHow::Merge => "merge",
        MergeHow::UpToDate => "upToDate",
    }
}

/// A stored accept as the protocol's merge.
pub(super) fn merge(accept: &parallax_store::RunAccept) -> AgentMerge {
    AgentMerge {
        commit: accept.commit.clone(),
        into: accept.into.clone(),
        how: match accept.how.as_str() {
            "fastForward" => AgentMergeKind::FastForward,
            "merge" => AgentMergeKind::Merge,
            "upToDate" => AgentMergeKind::UpToDate,
            _ => AgentMergeKind::Unknown,
        },
    }
}

fn diff(state: &parallax_store::RunState) -> Option<DiffSummary> {
    match (
        &state.commit_sha,
        state.files_changed,
        state.insertions,
        state.deletions,
    ) {
        (Some(commit), Some(files), Some(insertions), Some(deletions)) => Some(DiffSummary {
            commit: commit.clone(),
            files,
            insertions,
            deletions,
        }),
        _ => None,
    }
}

pub(super) fn failure_kind(kind: FailureKind) -> AgentFailureKind {
    match kind {
        FailureKind::NotSignedIn => AgentFailureKind::NotSignedIn,
        FailureKind::RateLimited => AgentFailureKind::RateLimited,
        FailureKind::PolicyViolation => AgentFailureKind::PolicyViolation,
        FailureKind::UnexpectedApiKey => AgentFailureKind::UnexpectedApiKey,
        FailureKind::VendorError => AgentFailureKind::VendorError,
        FailureKind::Crashed => AgentFailureKind::Crashed,
        FailureKind::SpawnFailed => AgentFailureKind::SpawnFailed,
        FailureKind::Internal | FailureKind::Other => AgentFailureKind::Internal,
    }
}

/// A backend outcome as the protocol's, and the run status it leaves the run in.
pub(super) fn outcome(outcome: &Outcome) -> (AgentOutcome, &'static str, Option<String>) {
    match outcome {
        Outcome::Completed { result } => (
            AgentOutcome::Completed {
                result: result.clone(),
            },
            COMPLETED,
            None,
        ),
        Outcome::Cancelled => (AgentOutcome::Cancelled, CANCELLED, None),
        Outcome::Failed(failure) => (
            AgentOutcome::Failed {
                failure: failure_kind(failure.failure),
                message: failure.message.clone(),
            },
            FAILED,
            Some(failure.message.clone()),
        ),
        Outcome::Unknown => {
            let message = "the run ended in a way this plxd does not know".to_owned();
            (
                AgentOutcome::Failed {
                    failure: AgentFailureKind::Internal,
                    message: message.clone(),
                },
                FAILED,
                Some(message),
            )
        }
    }
}

fn tool_status(status: ToolStatus) -> AgentToolStatus {
    match status {
        ToolStatus::Ok => AgentToolStatus::Ok,
        ToolStatus::Error => AgentToolStatus::Error,
        ToolStatus::Denied => AgentToolStatus::Denied,
        ToolStatus::Other => AgentToolStatus::Unknown,
    }
}

fn todo_status(status: TodoStatus) -> AgentTodoStatus {
    match status {
        TodoStatus::Pending => AgentTodoStatus::Pending,
        TodoStatus::InProgress => AgentTodoStatus::InProgress,
        TodoStatus::Completed => AgentTodoStatus::Completed,
        TodoStatus::Other => AgentTodoStatus::Unknown,
    }
}

/// `text` cut to at most `max` bytes on a character boundary, with a note when it was cut.
pub(super) fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n... ({} bytes cut)", &text[..end], text.len() - end)
}

/// A vendor-reported `message_id`, capped like any other identifier (#190 review).
fn id(message_id: Option<&str>) -> Option<String> {
    message_id.map(|id| truncate(id, MAX_ID_BYTES))
}

fn tool_input(input: &Value) -> Value {
    capped_input(input, MAX_TOOL_INPUT_BYTES)
}

/// `input`, or `{"truncated": true, "bytes": n}` when its JSON is over `max` bytes.
fn capped_input(input: &Value, max: usize) -> Value {
    let bytes = serde_json::to_string(input).map_or(0, |json| json.len());
    if bytes > max {
        json!({"truncated": true, "bytes": bytes})
    } else {
        input.clone()
    }
}

/// `items`, each truncated to `MAX_TODO_TEXT_BYTES`, kept only while the list's running total
/// stays within `MAX_TODO_LIST_BYTES` (#190 review): the total is counted in JSON-escaped bytes
/// (`escaped_len`, as `serde_json` writes a string), not raw ones, so text that needs a lot of
/// escaping can't make the list's real JSON size exceed the cap. Always keeps at least one item,
/// matching how the log's own paging never returns an empty, non-progressing page.
fn capped_todo_items(items: &[TodoItem]) -> Vec<AgentTodoItem> {
    let mut capped = Vec::new();
    let mut bytes = 0;
    for item in items {
        let text = truncate(&item.text, MAX_TODO_TEXT_BYTES);
        // + 2 for the quotes JSON puts around the string; a rough but conservative stand-in for
        // the rest of the item's own JSON (the status field, braces, and separators).
        let size = escaped_len(&text) + 2;
        if !capped.is_empty() && bytes + size > MAX_TODO_LIST_BYTES {
            break;
        }
        bytes += size;
        capped.push(AgentTodoItem {
            text,
            status: todo_status(item.status),
        });
    }
    capped
}

/// The transcript item for a backend event, or `None` for an event that isn't part of the
/// transcript: limits, fallbacks, and the run's end, which the runner reports on their own.
pub(super) fn output_item(event: &Event) -> Option<AgentOutputItem> {
    Some(match event {
        Event::SessionStarted {
            session_id, model, ..
        } => AgentOutputItem::SessionStarted {
            session_id: truncate(session_id, MAX_ID_BYTES),
            model: model.as_deref().map(|model| truncate(model, MAX_ID_BYTES)),
        },
        // The backend knows only the id; the run's actor adds a follow-up's text (RYA-92), marks
        // a coordinator's wake-up (RYA-42), and lists the message's images (RYA-191) and
        // attached threads (PLX-372).
        Event::TurnStarted { turn_id } => AgentOutputItem::TurnStarted {
            turn_id: *turn_id,
            text: None,
            wake: false,
            from: None,
            images: Vec::new(),
            threads: Vec::new(),
        },
        Event::TextDelta { message_id, text } => AgentOutputItem::TextDelta {
            message_id: id(message_id.as_deref()),
            text: truncate(text, MAX_TEXT_ITEM_BYTES),
        },
        Event::Text { message_id, text } => AgentOutputItem::Text {
            message_id: id(message_id.as_deref()),
            text: truncate(text, MAX_TEXT_ITEM_BYTES),
        },
        Event::ToolCall {
            call_id,
            name,
            input,
        } => AgentOutputItem::ToolCall {
            call_id: truncate(call_id, MAX_ID_BYTES),
            name: truncate(name, MAX_ID_BYTES),
            input: tool_input(input),
        },
        Event::ToolResult {
            call_id,
            status,
            output,
        } => AgentOutputItem::ToolResult {
            call_id: truncate(call_id, MAX_ID_BYTES),
            status: tool_status(*status),
            output: output
                .as_deref()
                .map(|output| truncate(output, MAX_TOOL_OUTPUT_BYTES)),
        },
        Event::Reasoning { message_id, text } => AgentOutputItem::Reasoning {
            message_id: id(message_id.as_deref()),
            text: truncate(text, MAX_TEXT_ITEM_BYTES),
        },
        Event::TodoList { items } => AgentOutputItem::TodoList {
            items: capped_todo_items(items),
        },
        Event::Notice { detail } => AgentOutputItem::Notice {
            detail: truncate(detail, MAX_TEXT_ITEM_BYTES),
        },
        Event::Usage(delta) => AgentOutputItem::Usage {
            model: delta.model.clone(),
            input_tokens: delta.usage.input_tokens,
            output_tokens: delta.usage.output_tokens,
            cache_read_tokens: delta.usage.cache_read_tokens,
            cache_write_tokens: delta.usage.cache_write_tokens,
            cost_usd_micros: delta.usage.cost_usd_micros,
        },
        Event::TurnFinished { turn_id, result } => AgentOutputItem::TurnFinished {
            turn_id: *turn_id,
            result: result
                .as_deref()
                .map(|result| truncate(result, MAX_TEXT_ITEM_BYTES)),
        },
        Event::FollowUpDropped { turn_id } => {
            AgentOutputItem::FollowUpDropped { turn_id: *turn_id }
        }
        Event::Warning { detail, .. } => AgentOutputItem::Warning {
            detail: truncate(detail, MAX_TEXT_ITEM_BYTES),
        },
        // The run's actor reports permission requests, with when they expire and how they end.
        Event::ApprovalRequested(_)
        | Event::ApprovalWithdrawn { .. }
        | Event::RateLimit(_)
        | Event::AccountFallback { .. }
        | Event::Finished { .. }
        | Event::Unknown => return None,
    })
}

/// The transcript item for a permission request that plxd denies at `expires_at` if nobody
/// answers it (RYA-222).
pub(super) fn approval_requested(
    request: &ApprovalRequest,
    expires_at: Timestamp,
) -> AgentOutputItem {
    let capped = |text: Option<&str>| text.map(|text| truncate(text, MAX_TEXT_ITEM_BYTES));
    AgentOutputItem::ApprovalRequested {
        approval_id: request.approval_id,
        tool_name: truncate(&request.tool_name, MAX_ID_BYTES),
        input: capped_input(&request.input, MAX_APPROVAL_INPUT_BYTES),
        call_id: id(request.call_id.as_deref()),
        reason: capped(request.reason.as_deref()),
        blocked_path: capped(request.blocked_path.as_deref()),
        subagent: id(request.subagent.as_deref()),
        always_allow: request
            .always_allow
            .iter()
            .take(MAX_ALWAYS_ALLOW_RULES)
            .map(|rule| truncate(rule, MAX_ALWAYS_ALLOW_RULE_BYTES))
            .collect(),
        interactive: request.interactive,
        expires_at,
    }
}

/// The transcript item for how a permission request ended (RYA-222).
pub(super) fn approval_resolved(
    approval_id: ApprovalId,
    resolution: &AgentApproveResult,
) -> AgentOutputItem {
    AgentOutputItem::ApprovalResolved {
        approval_id,
        decision: resolution.decision,
        by: resolution.by,
        always: resolution.always,
        message: resolution
            .message
            .as_deref()
            .map(|message| truncate(message, MAX_TEXT_ITEM_BYTES)),
    }
}

/// About how many bytes `item` adds to an `agent.output` event.
pub(super) fn item_bytes(item: &AgentOutputItem) -> usize {
    serde_json::to_string(item).map_or(0, |json| json.len())
}

#[cfg(test)]
mod tests {
    use parallax_protocol::{AgentOutputItem, AgentToolStatus};
    use serde_json::json;

    use super::{
        MAX_APPROVAL_INPUT_BYTES, MAX_ID_BYTES, MAX_TEXT_ITEM_BYTES, MAX_TODO_LIST_BYTES,
        MAX_TODO_TEXT_BYTES, MAX_TOOL_INPUT_BYTES, MAX_TOOL_OUTPUT_BYTES, approval_requested,
        output_item, truncate,
    };
    use crate::backend::{
        ApprovalId, ApprovalRequest, Event, LimitStatus, LimitWindow, ModelUsage, TodoItem,
        TodoStatus, ToolStatus, Usage,
    };

    /// RYA-222: a permission request carries a larger input than a tool call, so the user sees a
    /// long plan whole, and is cut like every other item past that. The actor logs requests
    /// itself, with when they expire, so they're no plain transcript item.
    #[test]
    fn a_permission_request_carries_more_input_than_a_tool_call() {
        let plan = "p".repeat(MAX_TOOL_INPUT_BYTES * 2);
        let request = ApprovalRequest {
            approval_id: ApprovalId::generate(),
            tool_name: "ExitPlanMode".into(),
            input: json!({"plan": plan}),
            call_id: None,
            reason: Some("r".repeat(MAX_TEXT_ITEM_BYTES + 1)),
            blocked_path: None,
            subagent: None,
            always_allow: (0..100).map(|n| format!("Bash(make {n}:*)")).collect(),
            interactive: true,
        };
        let expires_at = jiff::Timestamp::now();
        let AgentOutputItem::ApprovalRequested {
            input,
            reason,
            always_allow,
            ..
        } = approval_requested(&request, expires_at)
        else {
            panic!("a permission request");
        };
        assert_eq!(input["plan"], plan.as_str());
        assert!(reason.unwrap().ends_with("bytes cut)"));
        assert_eq!(always_allow.len(), 16);
        let huge = ApprovalRequest {
            input: json!({"plan": "p".repeat(MAX_APPROVAL_INPUT_BYTES)}),
            ..request.clone()
        };
        let AgentOutputItem::ApprovalRequested { input, .. } =
            approval_requested(&huge, expires_at)
        else {
            panic!("a permission request");
        };
        assert_eq!(input["truncated"], true);
        assert_eq!(output_item(&Event::ApprovalRequested(request)), None);
    }

    #[test]
    fn transcript_events_map_and_bookkeeping_events_do_not() {
        let usage = Event::Usage(ModelUsage {
            model: Some("opus".into()),
            usage: Usage {
                input_tokens: 3,
                output_tokens: 1,
                ..Usage::default()
            },
        });
        assert_eq!(
            output_item(&usage),
            Some(AgentOutputItem::Usage {
                model: Some("opus".into()),
                input_tokens: 3,
                output_tokens: 1,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd_micros: None,
            })
        );
        let limit = Event::RateLimit(LimitWindow {
            window: "five_hour".into(),
            duration_minutes: None,
            status: LimitStatus::Allowed,
            used_percent: None,
            resets_at: None,
        });
        assert_eq!(output_item(&limit), None);
        assert_eq!(output_item(&Event::Unknown), None);
    }

    #[test]
    fn large_tool_inputs_and_outputs_are_cut() {
        let big = "x".repeat(MAX_TOOL_INPUT_BYTES + 1);
        let call = Event::ToolCall {
            call_id: "c".into(),
            name: "Write".into(),
            input: json!({"content": big}),
        };
        let Some(AgentOutputItem::ToolCall { input, .. }) = output_item(&call) else {
            panic!("a tool call");
        };
        assert_eq!(input["truncated"], true);

        let result = Event::ToolResult {
            call_id: "c".into(),
            status: ToolStatus::Ok,
            output: Some("é".repeat(MAX_TOOL_OUTPUT_BYTES)),
        };
        let Some(AgentOutputItem::ToolResult { output, status, .. }) = output_item(&result) else {
            panic!("a tool result");
        };
        assert_eq!(status, AgentToolStatus::Ok);
        let output = output.unwrap();
        assert!(
            output.len() < MAX_TOOL_OUTPUT_BYTES + 64,
            "{}",
            output.len()
        );
        assert!(output.ends_with("bytes cut)"));
        assert_eq!(truncate("short", 10), "short");
    }

    /// #190 N8: every free text field of an `agent.output` item is capped, not only a tool's.
    #[test]
    fn oversized_text_fields_are_cut_to_the_same_cap_as_tool_output() {
        let big = "x".repeat(MAX_TEXT_ITEM_BYTES + 1);
        let cut = |field: &str| assert!(field.len() < MAX_TEXT_ITEM_BYTES + 64, "{}", field.len());

        let Some(AgentOutputItem::TextDelta { text, .. }) = output_item(&Event::TextDelta {
            message_id: None,
            text: big.clone(),
        }) else {
            panic!("a text delta");
        };
        cut(&text);

        let Some(AgentOutputItem::Text { text, .. }) = output_item(&Event::Text {
            message_id: None,
            text: big.clone(),
        }) else {
            panic!("text");
        };
        cut(&text);

        let Some(AgentOutputItem::Reasoning { text, .. }) = output_item(&Event::Reasoning {
            message_id: None,
            text: big.clone(),
        }) else {
            panic!("reasoning");
        };
        cut(&text);

        let Some(AgentOutputItem::Notice { detail }) = output_item(&Event::Notice {
            detail: big.clone(),
        }) else {
            panic!("a notice");
        };
        cut(&detail);

        let Some(AgentOutputItem::Warning { detail }) = output_item(&Event::Warning {
            warning: crate::backend::WarningKind::Other,
            detail: big.clone(),
        }) else {
            panic!("a warning");
        };
        cut(&detail);

        let Some(AgentOutputItem::TurnFinished { result, .. }) =
            output_item(&Event::TurnFinished {
                turn_id: None,
                result: Some(big),
            })
        else {
            panic!("a turn finished");
        };
        cut(&result.unwrap());
    }

    /// #190 review: identifiers are capped too, not only free text — a vendor bug or a hostile
    /// CLI reporting a huge `call_id`, `name`, `session_id`, `model`, or `message_id` shouldn't be
    /// able to blow up an `agent.output` event any more than a huge tool output can.
    #[test]
    fn oversized_identifiers_are_cut() {
        let big = "i".repeat(MAX_ID_BYTES + 1);
        let cut = |field: &str| assert!(field.len() < MAX_ID_BYTES + 64, "{}", field.len());

        let Some(AgentOutputItem::SessionStarted { session_id, model }) =
            output_item(&Event::SessionStarted {
                session_id: big.clone(),
                model: Some(big.clone()),
                api_key_source: None,
            })
        else {
            panic!("a session started");
        };
        cut(&session_id);
        cut(&model.unwrap());

        let Some(AgentOutputItem::ToolCall { call_id, name, .. }) = output_item(&Event::ToolCall {
            call_id: big.clone(),
            name: big.clone(),
            input: json!({}),
        }) else {
            panic!("a tool call");
        };
        cut(&call_id);
        cut(&name);

        let Some(AgentOutputItem::ToolResult { call_id, .. }) = output_item(&Event::ToolResult {
            call_id: big.clone(),
            status: ToolStatus::Ok,
            output: None,
        }) else {
            panic!("a tool result");
        };
        cut(&call_id);

        let Some(AgentOutputItem::Text { message_id, .. }) = output_item(&Event::Text {
            message_id: Some(big),
            text: "hi".to_owned(),
        }) else {
            panic!("text");
        };
        cut(&message_id.unwrap());
    }

    /// #190 review: a `TodoList` is capped on both axes — each item's text, and how many items
    /// one event carries — so neither a single huge item nor an unbounded count of short ones can
    /// make the event approach 0007's frame. The list's total is a JSON-escaped byte budget, not
    /// a raw one or a plain item count, so text that needs a lot of escaping is charged for what
    /// it actually costs once serialized.
    #[test]
    fn a_todo_list_is_capped_on_item_text_and_on_total_escaped_bytes() {
        let big_text = "t".repeat(MAX_TODO_TEXT_BYTES + 1);
        let plain: Vec<TodoItem> = (0..MAX_TODO_LIST_BYTES)
            .map(|i| TodoItem {
                text: if i == 0 {
                    big_text.clone()
                } else {
                    "task".to_owned()
                },
                status: TodoStatus::Pending,
            })
            .collect();
        let Some(AgentOutputItem::TodoList { items: plain }) =
            output_item(&Event::TodoList { items: plain })
        else {
            panic!("a todo list");
        };
        assert!(
            !plain.is_empty() && plain.len() < MAX_TODO_LIST_BYTES,
            "extra items are dropped once the total budget is spent: {}",
            plain.len()
        );
        assert!(
            plain[0].text.len() < MAX_TODO_TEXT_BYTES + 64,
            "{}",
            plain[0].text.len()
        );
        assert!(plain[0].text.ends_with("bytes cut)"));

        // Escape-heavy text costs more once serialized (each `"` becomes `\"`), so fewer such
        // items fit under the same budget than plain ones would: a raw byte count would let this
        // list's real JSON size run past MAX_TODO_LIST_BYTES.
        let escaped: Vec<TodoItem> = (0..MAX_TODO_LIST_BYTES)
            .map(|_| TodoItem {
                text: "\"\"\"\"".to_owned(),
                status: TodoStatus::Pending,
            })
            .collect();
        let Some(AgentOutputItem::TodoList { items: escaped }) =
            output_item(&Event::TodoList { items: escaped })
        else {
            panic!("a todo list");
        };
        assert!(
            escaped.len() < plain.len(),
            "escaped: {}, plain: {}",
            escaped.len(),
            plain.len()
        );
    }

    /// #190 review: `truncate` must land on a character boundary even when the cut falls inside a
    /// multibyte character, not only for the all-ASCII strings the other tests use.
    #[test]
    fn truncate_keeps_multibyte_text_valid_utf8_at_the_boundary() {
        // Each "é" is 2 bytes, so a byte cap that isn't a multiple of 2 lands mid-character;
        // `truncate` must still produce valid UTF-8 (guaranteed by `String`'s own invariant, so a
        // wrong cut point would panic rather than silently corrupt anything) and actually cut.
        let text: String = "é".repeat(20_000);
        let cut = truncate(&text, MAX_TEXT_ITEM_BYTES + 1);
        assert!(cut.len() < text.len());
        assert!(cut.ends_with("bytes cut)"));
    }
}
