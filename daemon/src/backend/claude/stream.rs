//! Turning Claude Code's `stream-json` output into [`Event`]s, one line at a time.
//!
//! The shapes follow the Agent SDK's types (0004 [14]) and the headless docs (0004 [11]). Fields
//! wisp doesn't use are ignored, as 0004 asks, so a newer CLI that adds fields still parses.

use std::collections::HashSet;

use jiff::Timestamp;
use serde_json::{Map, Value};

use super::{NO_WRITE_TOOLS, WORKER_TOOLS};
use crate::backend::ToolPolicy;
use crate::backend::event::{
    Event, Failure, FailureKind, LimitStatus, LimitWindow, ModelUsage, TodoItem, TodoStatus,
    ToolStatus, Usage, WarningKind,
};

/// The tool that only ends the session, which `--tools` leaves in place (the CLI reference).
const END_CONVERSATION: &str = "EndConversation";

/// The provider `result.modelUsage` names for Anthropic's own API, which a subscription uses.
const FIRST_PARTY: &str = "firstParty";

/// `result` subtypes for limits the run set itself, which say nothing about the account.
const RUN_LIMITS: &[&str] = &[
    "error_max_turns",
    "error_max_budget_usd",
    "error_max_structured_output_retries",
];

/// Message types that carry nothing wisp shows, and are skipped without a warning.
const IGNORED_TYPES: &[&str] = &[
    "auth_status",
    "conversation_reset",
    "keep_alive",
    "prompt_suggestion",
    "stream_event",
    "tool_progress",
    "tool_use_summary",
];

/// The longest failure message kept from the CLI's output.
const MAX_MESSAGE_CHARS: usize = 2000;

/// What one line of output asks the driver to do.
#[derive(Debug, PartialEq)]
pub(super) enum Step {
    /// Send an event.
    Emit(Event),
    /// A model's running total for the session, which becomes a usage delta.
    Total(ModelUsage),
    /// A `result`: a turn, or several the CLI folded into one, ended.
    TurnDone(TurnDone),
    /// The run broke its account or tool policy. The driver stops the CLI at once.
    Violation(Failure),
}

/// A `result` message, for the driver's turn bookkeeping.
#[derive(Debug, PartialEq)]
pub(super) struct TurnDone {
    /// The `uuid`s of the user messages the turn consumed, oldest first.
    pub uuids: Vec<String>,
    /// How many sent messages the CLI still had queued, when it says.
    pub queued: Option<u64>,
    /// The turn's final text, when it succeeded.
    pub result: Option<String>,
}

/// The state that reading one run's output needs.
#[derive(Debug)]
pub(super) struct Translator {
    policy: ToolPolicy,
    expected_key_source: &'static str,
    verified: bool,
    session_id: Option<String>,
    denied: HashSet<String>,
    turn_error: Option<FailureKind>,
    limit_rejected: bool,
    /// How many `result`s arrived.
    pub results: usize,
    /// The last `result`'s failure, if it was one.
    pub last_failure: Option<Failure>,
    /// The last successful `result`'s text.
    pub last_result: Option<String>,
}

impl Translator {
    /// A translator for a run under `policy` whose `system/init` must report
    /// `expected_key_source` as its `apiKeySource`.
    pub fn new(policy: ToolPolicy, expected_key_source: &'static str) -> Self {
        Self {
            policy,
            expected_key_source,
            verified: false,
            session_id: None,
            denied: HashSet::new(),
            turn_error: None,
            limit_rejected: false,
            results: 0,
            last_failure: None,
            last_result: None,
        }
    }

    /// Reads one line of stdout.
    pub fn line(&mut self, line: &[u8]) -> Vec<Step> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return Vec::new();
        }
        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(error) => return vec![warning(WarningKind::MalformedLine, error.to_string())],
        };
        let Value::Object(message) = value else {
            return vec![warning(
                WarningKind::MalformedLine,
                "a line that is not a JSON object".into(),
            )];
        };
        match text(&message, "type") {
            Some("system") => self.system(&message),
            Some("assistant") => self.assistant(&message),
            Some("user") => self.user(&message),
            Some("result") => self.result(&message),
            Some("rate_limit_event") => self.rate_limit(&message),
            Some(kind) if IGNORED_TYPES.contains(&kind) => Vec::new(),
            Some(kind) => vec![warning(
                WarningKind::UnknownEvent,
                format!("a message of type {kind:?}"),
            )],
            None => vec![warning(
                WarningKind::MalformedLine,
                "a message without a type".into(),
            )],
        }
    }

    fn system(&mut self, message: &Map<String, Value>) -> Vec<Step> {
        match text(message, "subtype") {
            Some("init") => self.init(message),
            Some("permission_denied") => {
                if let Some(id) = text(message, "tool_use_id") {
                    self.denied.insert(id.to_owned());
                }
                Vec::new()
            }
            Some("api_retry") => {
                let error = text(message, "error").unwrap_or("unknown");
                self.turn_error = Some(api_error_kind(error));
                let attempt = message.get("attempt").and_then(Value::as_u64);
                let max = message.get("max_retries").and_then(Value::as_u64);
                let detail = match (attempt, max) {
                    (Some(attempt), Some(max)) => format!(
                        "Claude Code is retrying a request after an error ({error}), attempt {attempt} of {max}"
                    ),
                    _ => format!("Claude Code is retrying a request after an error ({error})"),
                };
                vec![Step::Emit(Event::Notice { detail })]
            }
            _ => Vec::new(),
        }
    }

    /// `system/init`, which stream-json input repeats at the start of every turn.
    fn init(&mut self, message: &Map<String, Value>) -> Vec<Step> {
        let Some(session_id) = text(message, "session_id") else {
            return vec![warning(
                WarningKind::MalformedLine,
                "an init message without a session id".into(),
            )];
        };
        let source = text(message, "apiKeySource");
        let mut steps = Vec::new();
        if self.session_id.as_deref() != Some(session_id) {
            self.session_id = Some(session_id.to_owned());
            steps.push(Step::Emit(Event::SessionStarted {
                session_id: session_id.to_owned(),
                model: text(message, "model").map(str::to_owned),
                api_key_source: source.map(str::to_owned),
            }));
        }
        if source != Some(self.expected_key_source) {
            let message = match source {
                Some(source) => format!(
                    "Claude Code took its credentials from {source:?} instead of the account's \
                     ({:?}), so wisp stopped it before it could bill that",
                    self.expected_key_source
                ),
                None => "Claude Code did not say where it took its credentials from, so wisp \
                         stopped it"
                    .to_owned(),
            };
            steps.push(violation(FailureKind::UnexpectedApiKey, message));
            return steps;
        }
        let (allowed, run) = match self.policy {
            ToolPolicy::NoWrite => (NO_WRITE_TOOLS, "a no-write run"),
            ToolPolicy::WorkspaceWrite => (WORKER_TOOLS, "a worker run"),
        };
        let tools = message.get("tools").and_then(Value::as_array);
        let Some(tools) = tools else {
            let message = format!("Claude Code did not list its tools in {run}");
            steps.push(violation(FailureKind::PolicyViolation, message));
            return steps;
        };
        let offered: Vec<&str> = tools
            .iter()
            .map(|tool| tool.as_str().unwrap_or("<not a string>"))
            .filter(|tool| !allowed.contains(tool) && *tool != END_CONVERSATION)
            .collect();
        if !offered.is_empty() {
            let message = format!(
                "Claude Code offered tools beyond {} in {run}: {}",
                allowed.join(", "),
                offered.join(", ")
            );
            steps.push(violation(FailureKind::PolicyViolation, message));
            return steps;
        }
        self.verified = true;
        steps
    }

    fn assistant(&mut self, message: &Map<String, Value>) -> Vec<Step> {
        if !self.verified {
            return vec![unverified()];
        }
        if let Some(error) = text(message, "error") {
            self.turn_error = Some(api_error_kind(error));
        }
        let Some(inner) = message.get("message").and_then(Value::as_object) else {
            return vec![warning(
                WarningKind::MalformedLine,
                "an assistant message without a message".into(),
            )];
        };
        let message_id = text(inner, "id").map(str::to_owned);
        let blocks = inner.get("content").and_then(Value::as_array);
        let mut steps = Vec::new();
        for block in blocks.into_iter().flatten().filter_map(Value::as_object) {
            match text(block, "type") {
                Some("text") => {
                    if let Some(text) = text(block, "text").filter(|text| !text.is_empty()) {
                        steps.push(Step::Emit(Event::Text {
                            message_id: message_id.clone(),
                            text: text.to_owned(),
                        }));
                    }
                }
                Some("thinking") => {
                    if let Some(text) = text(block, "thinking").filter(|text| !text.is_empty()) {
                        steps.push(Step::Emit(Event::Reasoning {
                            message_id: message_id.clone(),
                            text: text.to_owned(),
                        }));
                    }
                }
                Some("tool_use") => {
                    let (Some(id), Some(name)) = (text(block, "id"), text(block, "name")) else {
                        steps.push(warning(
                            WarningKind::MalformedLine,
                            "a tool call without an id or a name".into(),
                        ));
                        continue;
                    };
                    let input = block.get("input").cloned().unwrap_or(Value::Null);
                    let todos = (name == "TodoWrite").then(|| todo_list(&input)).flatten();
                    steps.push(Step::Emit(Event::ToolCall {
                        call_id: id.to_owned(),
                        name: name.to_owned(),
                        input,
                    }));
                    if let Some(items) = todos {
                        steps.push(Step::Emit(Event::TodoList { items }));
                    }
                }
                _ => {}
            }
        }
        steps
    }

    fn user(&mut self, message: &Map<String, Value>) -> Vec<Step> {
        let blocks = message
            .get("message")
            .and_then(|inner| inner.get("content"))
            .and_then(Value::as_array);
        let mut steps = Vec::new();
        for block in blocks.into_iter().flatten().filter_map(Value::as_object) {
            if text(block, "type") != Some("tool_result") {
                continue;
            }
            let Some(call_id) = text(block, "tool_use_id") else {
                steps.push(warning(
                    WarningKind::MalformedLine,
                    "a tool result without a tool_use_id".into(),
                ));
                continue;
            };
            let status = if self.denied.remove(call_id) {
                ToolStatus::Denied
            } else if block.get("is_error").and_then(Value::as_bool) == Some(true) {
                ToolStatus::Error
            } else {
                ToolStatus::Ok
            };
            steps.push(Step::Emit(Event::ToolResult {
                call_id: call_id.to_owned(),
                status,
                output: block.get("content").and_then(tool_output),
            }));
        }
        steps
    }

    fn result(&mut self, message: &Map<String, Value>) -> Vec<Step> {
        let subtype = text(message, "subtype").unwrap_or_default();
        let failed =
            subtype != "success" || message.get("is_error").and_then(Value::as_bool) == Some(true);
        if !self.verified && !failed {
            return vec![unverified()];
        }
        let totals = message.get("modelUsage").and_then(Value::as_object);
        let providers: Vec<&str> = totals
            .into_iter()
            .flatten()
            .filter_map(|(_, usage)| usage.get("provider").and_then(Value::as_str))
            .filter(|provider| *provider != FIRST_PARTY)
            .collect();
        if !providers.is_empty() {
            let message = format!(
                "Claude Code used the model through {} instead of Anthropic's API, so the run was \
                 not charged to the account",
                providers.join(", ")
            );
            return vec![violation(FailureKind::UnexpectedApiKey, message)];
        }
        self.results += 1;
        let result = text(message, "result").map(str::to_owned);
        let mut steps = Vec::new();
        for (model, usage) in totals.into_iter().flatten() {
            let usage = model_usage(usage);
            if !is_zero(&usage) {
                steps.push(Step::Total(ModelUsage {
                    model: Some(model.clone()),
                    usage,
                }));
            }
        }
        if failed {
            let failure = if RUN_LIMITS.contains(&subtype) {
                FailureKind::VendorError
            } else if self.limit_rejected {
                FailureKind::RateLimited
            } else if let Some(kind) = self.turn_error {
                kind
            } else if result.as_deref().is_some_and(signed_out) {
                FailureKind::NotSignedIn
            } else {
                FailureKind::VendorError
            };
            let errors: Vec<&str> = message
                .get("errors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let text = match (&result, errors.is_empty()) {
                (Some(result), _) if !result.is_empty() => result.clone(),
                (_, false) => errors.join("; "),
                _ => format!("Claude Code ended the turn with {subtype:?}"),
            };
            self.last_failure = Some(Failure {
                failure,
                message: truncate(&text),
                exit: None,
                stderr_tail: None,
            });
            self.last_result = None;
        } else {
            self.last_failure = None;
            self.last_result.clone_from(&result);
        }
        self.turn_error = None;
        self.limit_rejected = false;
        let uuids = match message.get("user_message_uuids").and_then(Value::as_array) {
            Some(uuids) => uuids
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect(),
            None => text(message, "user_message_uuid")
                .map(str::to_owned)
                .into_iter()
                .collect(),
        };
        steps.push(Step::TurnDone(TurnDone {
            uuids,
            queued: message.get("queued_turn_count").and_then(Value::as_u64),
            result: if failed { None } else { result },
        }));
        steps
    }

    fn rate_limit(&mut self, message: &Map<String, Value>) -> Vec<Step> {
        let Some(info) = message.get("rate_limit_info").and_then(Value::as_object) else {
            return vec![warning(
                WarningKind::MalformedLine,
                "a rate limit event without rate_limit_info".into(),
            )];
        };
        let status = match text(info, "status") {
            Some("allowed") => LimitStatus::Allowed,
            Some("allowed_warning") => LimitStatus::Warning,
            Some("rejected") => LimitStatus::Rejected,
            _ => LimitStatus::Unknown,
        };
        self.limit_rejected |= status == LimitStatus::Rejected;
        let window = text(info, "rateLimitType").unwrap_or("unknown");
        let duration_minutes = match window {
            "five_hour" => Some(5 * 60),
            window if window.starts_with("seven_day") => Some(7 * 24 * 60),
            _ => None,
        };
        vec![Step::Emit(Event::RateLimit(LimitWindow {
            window: window.to_owned(),
            duration_minutes,
            used_percent: info.get("utilization").and_then(Value::as_f64).map(percent),
            status,
            resets_at: info.get("resetsAt").and_then(timestamp),
        }))]
    }
}

fn text<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn warning(warning: WarningKind, detail: String) -> Step {
    Step::Emit(Event::Warning { warning, detail })
}

fn violation(failure: FailureKind, message: String) -> Step {
    Step::Violation(Failure {
        failure,
        message,
        exit: None,
        stderr_tail: None,
    })
}

/// Output before a `system/init` passed the credential check: the CLI may already be using
/// credentials nobody checked.
fn unverified() -> Step {
    violation(
        FailureKind::UnexpectedApiKey,
        "Claude Code answered before it reported its credentials, so wisp stopped it".into(),
    )
}

/// The failure an `assistant` message's `error` points to.
fn api_error_kind(error: &str) -> FailureKind {
    match error {
        "authentication_failed" | "oauth_org_not_allowed" => FailureKind::NotSignedIn,
        "rate_limit" => FailureKind::RateLimited,
        _ => FailureKind::VendorError,
    }
}

/// Whether a failed turn's text says the CLI isn't signed in, which the headless docs say is
/// reported as the result (0004 [11]).
fn signed_out(result: &str) -> bool {
    let result = result.to_ascii_lowercase();
    result.contains("not logged in") || result.contains("/login")
}

/// A `tool_result`'s content: a string, or text blocks.
fn tool_output(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => return None,
    };
    (!text.is_empty()).then_some(text)
}

/// `TodoWrite`'s `todos`, as a checklist.
fn todo_list(input: &Value) -> Option<Vec<TodoItem>> {
    let todos = input.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|todo| {
                Some(TodoItem {
                    text: todo.get("content")?.as_str()?.to_owned(),
                    status: match todo.get("status").and_then(Value::as_str) {
                        Some("pending") => TodoStatus::Pending,
                        Some("in_progress") => TodoStatus::InProgress,
                        Some("completed") => TodoStatus::Completed,
                        _ => TodoStatus::Other,
                    },
                })
            })
            .collect(),
    )
}

/// One entry of `result.modelUsage`.
fn model_usage(usage: &Value) -> Usage {
    let count = |key| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: count("inputTokens"),
        output_tokens: count("outputTokens"),
        cache_read_tokens: count("cacheReadInputTokens"),
        cache_write_tokens: count("cacheCreationInputTokens"),
        cost_usd_micros: usage
            .get("costUSD")
            .and_then(Value::as_f64)
            .and_then(micros),
    }
}

/// Whether a total counts nothing. A result after a crash can carry zeroed totals (0004 [15]);
/// taking those as the session's totals would count everything again on the next resume.
fn is_zero(usage: &Usage) -> bool {
    usage.input_tokens == 0
        && usage.output_tokens == 0
        && usage.cache_read_tokens == 0
        && usage.cache_write_tokens == 0
        && usage.cost_usd_micros.unwrap_or(0) == 0
}

#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a finite, non-negative cost in dollars fits in u64 micros"
)]
fn micros(usd: f64) -> Option<u64> {
    (usd.is_finite() && usd >= 0.0).then(|| (usd * 1_000_000.0).round() as u64)
}

/// `utilization` as a percentage. The SDK types don't say its scale; `get_usage` reports 0 to
/// 100, while the rate limit event is taken to be a fraction from 0 to 1. Until a recorded
/// transcript settles it (#124), a value above 1 is read as a percentage already.
fn percent(utilization: f64) -> f64 {
    if utilization <= 1.0 {
        utilization * 100.0
    } else {
        utilization
    }
}

/// Epoch seconds, whole or fractional, to millisecond precision.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a finite number of milliseconds since 1970 fits in i64 for any date jiff accepts"
)]
fn timestamp(seconds: &Value) -> Option<Timestamp> {
    if let Some(seconds) = seconds.as_i64() {
        return Timestamp::from_second(seconds).ok();
    }
    let seconds = seconds.as_f64().filter(|seconds| seconds.is_finite())?;
    Timestamp::from_millisecond((seconds * 1000.0).round() as i64).ok()
}

fn truncate(text: &str) -> String {
    match text.char_indices().nth(MAX_MESSAGE_CHARS) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_owned(),
    }
}
