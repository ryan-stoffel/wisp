//! Mapping between the runner's inputs and outputs: backend events to the protocol's transcript
//! items, and store rows to the protocol's runs.

use serde_json::{Value, json};
use tracing::error;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AgentFailureKind, AgentMerge, AgentMergeKind, AgentOutcome, AgentOutputItem, AgentPolicy,
    AgentRun, AgentRunState, AgentStatus, AgentTodoItem, AgentTodoStatus, AgentToolStatus,
    DiffSummary, ProjectId, RunId,
};

use crate::backend::{Event, FailureKind, Outcome, TodoItem, TodoStatus, ToolStatus};
use crate::json::escaped_len;
use crate::worktree::MergeHow;

/// The longest free text field of an `agent.output` item (a tool's output, text, reasoning, a
/// notice, a turn's result), in bytes, and the largest tool input as JSON, which becomes
/// `{"truncated": true, "bytes": n}` past it. The full text stays in the CLI's own session, and
/// one item near 0007's 8 MiB frame would close every subscriber and be replayed on reconnect.
const MAX_TEXT_BYTES: usize = 32 * 1024;

/// The longest a vendor-assigned identifier gets to be, in bytes: call ids, tool names, session
/// ids, models, and message ids. Defense in depth against a buggy or hostile CLI.
const MAX_ID_BYTES: usize = 1024;

/// The longest one `TodoList` item's `text` gets to be, in bytes.
const MAX_TODO_TEXT_BYTES: usize = 4 * 1024;

/// The most a `TodoList`'s items add up to, in JSON-escaped bytes, since a raw count would
/// undercount text that needs escaping. Items past it are dropped.
const MAX_TODO_LIST_BYTES: usize = 64 * 1024;

pub(super) const STARTING: &str = "starting";
pub(super) const RUNNING: &str = "running";
pub(super) const COMPLETED: &str = "completed";
pub(super) const FAILED: &str = "failed";
pub(super) const CANCELLED: &str = "cancelled";
pub(super) const INTERRUPTED: &str = "interrupted";
pub(super) const ACCEPTED: &str = "accepted";

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
        ACCEPTED => AgentStatus::Accepted,
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
        diff: diff(state),
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

/// The part of a stored run that `agent.updated` reports.
pub(super) fn run_state(row: &wisp_store::Run) -> AgentRunState {
    let state = &row.state;
    AgentRunState {
        status: status(&state.status),
        account_id: state.account_id.clone(),
        session_id: state.session_id.clone(),
        error: state.error.clone(),
        diff: diff(state),
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
pub(super) fn merge(accept: &wisp_store::RunAccept) -> AgentMerge {
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

fn diff(state: &wisp_store::RunState) -> Option<DiffSummary> {
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

/// A vendor-reported `message_id`, capped like any other identifier.
fn id(message_id: Option<&str>) -> Option<String> {
    message_id.map(|id| truncate(id, MAX_ID_BYTES))
}

fn tool_input(input: &Value) -> Value {
    let bytes = serde_json::to_string(input).map_or(0, |json| json.len());
    if bytes > MAX_TEXT_BYTES {
        json!({"truncated": true, "bytes": bytes})
    } else {
        input.clone()
    }
}

/// `items`, each truncated to `MAX_TODO_TEXT_BYTES`, kept only while the list's escaped size
/// stays within `MAX_TODO_LIST_BYTES`. Always keeps at least one item.
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
        Event::TurnStarted { turn_id } => AgentOutputItem::TurnStarted { turn_id: *turn_id },
        Event::TextDelta { message_id, text } => AgentOutputItem::TextDelta {
            message_id: id(message_id.as_deref()),
            text: truncate(text, MAX_TEXT_BYTES),
        },
        Event::Text { message_id, text } => AgentOutputItem::Text {
            message_id: id(message_id.as_deref()),
            text: truncate(text, MAX_TEXT_BYTES),
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
                .map(|output| truncate(output, MAX_TEXT_BYTES)),
        },
        Event::Reasoning { message_id, text } => AgentOutputItem::Reasoning {
            message_id: id(message_id.as_deref()),
            text: truncate(text, MAX_TEXT_BYTES),
        },
        Event::TodoList { items } => AgentOutputItem::TodoList {
            items: capped_todo_items(items),
        },
        Event::Notice { detail } => AgentOutputItem::Notice {
            detail: truncate(detail, MAX_TEXT_BYTES),
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
                .map(|result| truncate(result, MAX_TEXT_BYTES)),
        },
        Event::FollowUpDropped { turn_id } => {
            AgentOutputItem::FollowUpDropped { turn_id: *turn_id }
        }
        Event::Warning { detail, .. } => AgentOutputItem::Warning {
            detail: truncate(detail, MAX_TEXT_BYTES),
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
    use wisp_protocol::AgentOutputItem;

    use super::{
        MAX_ID_BYTES, MAX_TEXT_BYTES, MAX_TODO_LIST_BYTES, MAX_TODO_TEXT_BYTES, item_bytes,
        output_item, truncate,
    };
    use crate::backend::{
        Event, LimitStatus, LimitWindow, ModelUsage, TodoItem, TodoStatus, ToolStatus, Usage,
        WarningKind,
    };

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
    fn every_free_text_field_identifier_and_tool_input_is_capped() {
        let big = "x".repeat(4 * MAX_TEXT_BYTES);
        let id = Some(big.clone());
        let events = [
            Event::SessionStarted {
                session_id: big.clone(),
                model: id.clone(),
                api_key_source: None,
            },
            Event::TextDelta {
                message_id: id.clone(),
                text: big.clone(),
            },
            Event::Text {
                message_id: id.clone(),
                text: big.clone(),
            },
            Event::Reasoning {
                message_id: id.clone(),
                text: big.clone(),
            },
            Event::ToolCall {
                call_id: big.clone(),
                name: big.clone(),
                input: json!({"content": big}),
            },
            Event::ToolResult {
                call_id: big.clone(),
                status: ToolStatus::Ok,
                output: id.clone(),
            },
            Event::Notice {
                detail: big.clone(),
            },
            Event::Warning {
                warning: WarningKind::Other,
                detail: big.clone(),
            },
            Event::TurnFinished {
                turn_id: None,
                result: id,
            },
        ];
        for (index, event) in events.iter().enumerate() {
            let bytes = item_bytes(&output_item(event).unwrap());
            assert!(
                bytes < MAX_TEXT_BYTES + 2 * MAX_ID_BYTES + 256,
                "event {index}: {bytes} bytes"
            );
        }
    }

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

        // Escape-heavy text costs more once serialized, so fewer such items fit.
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

    #[test]
    fn truncate_cuts_on_a_character_boundary() {
        assert_eq!(truncate("short", 10), "short");
        // Each "é" is 2 bytes, so an odd cap lands mid-character; a wrong cut would panic.
        let text: String = "é".repeat(20_000);
        let cut = truncate(&text, MAX_TEXT_BYTES + 1);
        assert!(cut.len() < text.len());
        assert!(cut.ends_with("bytes cut)"));
    }
}
