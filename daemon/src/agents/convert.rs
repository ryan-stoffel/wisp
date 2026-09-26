//! Mapping between the runner's inputs and outputs: backend events to the protocol's transcript
//! items, and store rows to the protocol's runs.

use serde_json::{Value, json};
use tracing::error;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AgentFailureKind, AgentOutcome, AgentOutputItem, AgentPolicy, AgentRun, AgentStatus,
    AgentTodoItem, AgentTodoStatus, AgentToolStatus, DiffSummary, ProjectId, RunId,
};

use crate::backend::{Event, FailureKind, Outcome, TodoStatus, ToolStatus};

/// The longest tool output an `agent.output` item carries, in bytes. The rest is cut, since the
/// full output stays in the CLI's own session and 0007 caps a frame at 8 MiB.
pub(super) const MAX_TOOL_OUTPUT_BYTES: usize = 32 * 1024;

/// The largest tool input an `agent.output` item carries as JSON, in bytes. A larger one, such as
/// a `Write` of a big file, becomes `{"truncated": true, "bytes": n}`.
pub(super) const MAX_TOOL_INPUT_BYTES: usize = 32 * 1024;

pub(super) const STARTING: &str = "starting";
pub(super) const RUNNING: &str = "running";
pub(super) const COMPLETED: &str = "completed";
pub(super) const FAILED: &str = "failed";
pub(super) const CANCELLED: &str = "cancelled";
pub(super) const INTERRUPTED: &str = "interrupted";

/// The store's text for the only policy `agent/start` takes.
pub(super) const WORKSPACE_WRITE: &str = "workspaceWrite";

fn status(text: &str) -> AgentStatus {
    match text {
        STARTING => AgentStatus::Starting,
        RUNNING => AgentStatus::Running,
        COMPLETED => AgentStatus::Completed,
        FAILED => AgentStatus::Failed,
        CANCELLED => AgentStatus::Cancelled,
        INTERRUPTED => AgentStatus::Interrupted,
        _ => AgentStatus::Unknown,
    }
}

/// A store row and its worktree as the protocol's run.
///
/// wispd writes only version 7 ids, so a row with another kind of id was written by something
/// else, and the request fails rather than hide the row.
pub(crate) fn agent_run(
    row: &wisp_store::Run,
    worktree: Option<&wisp_store::Worktree>,
) -> Result<AgentRun, ErrorObject> {
    let corrupt = |what: &str| {
        error!(run = %row.id, what, "a stored run has an invalid id");
        ErrorObject::internal_error(format!("the stored run {} has an invalid {what}", row.id))
    };
    let id = RunId::try_from(row.id).map_err(|_| corrupt("id"))?;
    let project = ProjectId::try_from(row.fields.project_id).map_err(|_| corrupt("project id"))?;
    let state = &row.state;
    let diff = match (
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
    };
    Ok(AgentRun {
        id,
        project,
        prompt: row.fields.prompt.clone(),
        policy: if row.fields.policy == WORKSPACE_WRITE {
            AgentPolicy::WorkspaceWrite
        } else {
            AgentPolicy::Unknown
        },
        status: status(&state.status),
        backend: row.fields.backend.clone(),
        account_id: state.account_id.clone(),
        branch: worktree.map(|worktree| worktree.branch.clone()),
        worktree_path: worktree.map(|worktree| worktree.path.clone()),
        session_id: state.session_id.clone(),
        error: state.error.clone(),
        diff,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
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
            let message = "the run ended in a way this wispd does not know".to_owned();
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

fn tool_input(input: &Value) -> Value {
    let bytes = serde_json::to_string(input).map_or(0, |json| json.len());
    if bytes > MAX_TOOL_INPUT_BYTES {
        json!({"truncated": true, "bytes": bytes})
    } else {
        input.clone()
    }
}

/// The transcript item for a backend event, or `None` for an event that isn't part of the
/// transcript: limits, fallbacks, and the run's end, which the runner reports on their own.
pub(super) fn output_item(event: &Event) -> Option<AgentOutputItem> {
    Some(match event {
        Event::SessionStarted {
            session_id, model, ..
        } => AgentOutputItem::SessionStarted {
            session_id: session_id.clone(),
            model: model.clone(),
        },
        Event::TurnStarted { turn_id } => AgentOutputItem::TurnStarted { turn_id: *turn_id },
        Event::TextDelta { message_id, text } => AgentOutputItem::TextDelta {
            message_id: message_id.clone(),
            text: text.clone(),
        },
        Event::Text { message_id, text } => AgentOutputItem::Text {
            message_id: message_id.clone(),
            text: text.clone(),
        },
        Event::ToolCall {
            call_id,
            name,
            input,
        } => AgentOutputItem::ToolCall {
            call_id: call_id.clone(),
            name: name.clone(),
            input: tool_input(input),
        },
        Event::ToolResult {
            call_id,
            status,
            output,
        } => AgentOutputItem::ToolResult {
            call_id: call_id.clone(),
            status: tool_status(*status),
            output: output
                .as_deref()
                .map(|output| truncate(output, MAX_TOOL_OUTPUT_BYTES)),
        },
        Event::Reasoning { message_id, text } => AgentOutputItem::Reasoning {
            message_id: message_id.clone(),
            text: text.clone(),
        },
        Event::TodoList { items } => AgentOutputItem::TodoList {
            items: items
                .iter()
                .map(|item| AgentTodoItem {
                    text: item.text.clone(),
                    status: todo_status(item.status),
                })
                .collect(),
        },
        Event::Notice { detail } => AgentOutputItem::Notice {
            detail: detail.clone(),
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
            result: result.clone(),
        },
        Event::FollowUpDropped { turn_id } => {
            AgentOutputItem::FollowUpDropped { turn_id: *turn_id }
        }
        Event::Warning { detail, .. } => AgentOutputItem::Warning {
            detail: detail.clone(),
        },
        Event::RateLimit(_)
        | Event::AccountFallback { .. }
        | Event::Finished { .. }
        | Event::Unknown => return None,
    })
}

/// About how many bytes `item` adds to an `agent.output` event.
pub(super) fn item_bytes(item: &AgentOutputItem) -> usize {
    serde_json::to_string(item).map_or(0, |json| json.len())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use wisp_protocol::{AgentOutputItem, AgentToolStatus};

    use super::{MAX_TOOL_INPUT_BYTES, MAX_TOOL_OUTPUT_BYTES, output_item, truncate};
    use crate::backend::{Event, LimitStatus, LimitWindow, ModelUsage, ToolStatus, Usage};

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
}
