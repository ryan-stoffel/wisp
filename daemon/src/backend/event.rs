//! The normalized events every backend turns its CLI's output into (0004's per-provider table).
//!
//! They serialize with a `kind` tag and camelCase fields, like 0007's `WispEvent`, so M3 can
//! store them and map them to `agent.*` notifications without another translation.

use std::collections::BTreeMap;
use std::ops::{Add, AddAssign};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use wisp_protocol::TurnId;

/// One thing that happened in a run.
///
/// A run's stream ends with exactly one [`Event::Finished`], and nothing follows it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Event {
    /// The CLI started or resumed its session: Claude's and Cursor's `system/init`, Codex's
    /// `thread.started`.
    SessionStarted {
        /// The vendor's id for the session, which a later run passes as
        /// [`RunRequest::resume`](super::RunRequest::resume): Claude's `session_id`, Codex's
        /// `thread_id`, or Cursor's `chatId`.
        session_id: String,
        /// The model the CLI chose, when it says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Where the CLI took its credentials from, when it says, such as Claude's `apiKeySource`
        /// (`none` for a subscription login). A backend checks it before charging an account.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        api_key_source: Option<String>,
    },
    /// A follow-up turn began.
    TurnStarted {
        /// The follow-up's id from [`FollowUp`](super::FollowUp).
        turn_id: TurnId,
    },
    /// Part of the assistant's reply, as it streams.
    TextDelta {
        /// The vendor's id for the message it belongs to, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        /// The new text.
        text: String,
    },
    /// One whole assistant message.
    Text {
        /// The vendor's id for the message, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message_id: Option<String>,
        /// The message's text.
        text: String,
    },
    /// The agent called a tool.
    ToolCall {
        /// The vendor's id for the call, which its [`Event::ToolResult`] repeats.
        call_id: String,
        /// The tool, in the vendor's naming, such as `Read`, `command_execution`, or
        /// `mcp__wispd__plan`.
        name: String,
        /// The tool's input, as the vendor sent it.
        #[serde(default)]
        input: serde_json::Value,
    },
    /// A tool call ended.
    ToolResult {
        /// The call's id from its [`Event::ToolCall`].
        call_id: String,
        /// How it ended.
        status: ToolStatus,
        /// What the tool returned, when the vendor includes it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
    },
    /// Tokens and cost the run used since its previous `Usage` event.
    Usage(UsageDelta),
    /// The latest state of one of the account's limit windows.
    RateLimit(LimitWindow),
    /// A turn ended: Claude's and Cursor's `result`, Codex's `turn.completed`.
    TurnFinished {
        /// The follow-up's id, or absent for the turn that the run's prompt started.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<TurnId>,
        /// The turn's final text, when the vendor reports one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
    },
    /// A follow-up that [`Run::send`](super::Run::send) accepted never reached the CLI, because
    /// the run ended first. The caller can send it again in a run that resumes this session.
    FollowUpDropped {
        /// The follow-up's id.
        turn_id: TurnId,
    },
    /// Something in the CLI's output that the backend skipped. It never ends a run.
    Warning {
        /// What was wrong.
        warning: WarningKind,
        /// A short description, safe to log.
        detail: String,
    },
    /// The run ended. Always the last event.
    Finished {
        /// How it ended.
        outcome: Outcome,
    },
}

impl Event {
    /// Whether this is the run's last event.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Finished { .. })
    }
}

/// How a tool call ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolStatus {
    /// It ran and succeeded.
    Ok,
    /// It ran and failed.
    Error,
    /// The tool policy or the CLI's approval rules refused it: Claude's `permission_denied`, or a
    /// Codex approval request that exec's `never` policy denied.
    Denied,
    /// A status this version does not know.
    #[serde(other)]
    Other,
}

/// What a [`Event::Warning`] is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WarningKind {
    /// A line of the CLI's output that isn't valid JSON, or not the shape the backend expects.
    MalformedLine,
    /// A line longer than the backend's limit, which was skipped without being buffered.
    OversizedLine,
    /// A well-formed event of a type the backend doesn't know. 0004: adapters ignore those.
    UnknownEvent,
    /// A kind this version does not know.
    #[serde(other)]
    Other,
}

/// How a run ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Outcome {
    /// The CLI finished its work.
    Completed {
        /// The last turn's final text, when the vendor reports one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<String>,
    },
    /// [`Run::cancel`](super::Run::cancel) stopped it.
    Cancelled,
    /// It failed.
    Failed(Failure),
}

/// Why a run failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Failure {
    /// What went wrong, for code to match on.
    pub failure: FailureKind,
    /// A description for people, safe to log and show.
    pub message: String,
    /// How the process ended, when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<ExitInfo>,
    /// The end of the CLI's stderr, when it wrote any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr_tail: Option<String>,
}

/// What kind of failure ended a run. Routing (#119) falls back to another account on
/// [`FailureKind::NotSignedIn`] and [`FailureKind::RateLimited`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureKind {
    /// The CLI isn't signed in, or its login expired.
    NotSignedIn,
    /// The account hit a usage limit.
    RateLimited,
    /// The run broke the account or tool policy: for example a subscription run whose
    /// `apiKeySource` is not `none`, or a no-write turn that changed the repository.
    PolicyViolation,
    /// The CLI reported an error of its own, such as Claude's `error_max_turns`.
    VendorError,
    /// The process died from a signal, or exited unsuccessfully without saying why.
    Crashed,
    /// A process the run needed could not be started.
    SpawnFailed,
    /// A fault in wispd.
    Internal,
    /// A kind this version does not know.
    #[serde(other)]
    Other,
}

/// How a process ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitInfo {
    /// Its exit code, if it exited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    /// The signal that killed it, if one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

impl ExitInfo {
    /// Whether it exited with code 0.
    #[must_use]
    pub fn success(self) -> bool {
        self.code == Some(0)
    }
}

/// Token counts and cost. Every field adds up, so per-run and per-account totals are sums.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Input tokens, not counting cache reads and writes.
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens, including reasoning.
    #[serde(default)]
    pub output_tokens: u64,
    /// Input tokens read from the prompt cache.
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// Input tokens written to the prompt cache.
    #[serde(default)]
    pub cache_write_tokens: u64,
    /// The cost the vendor reported, in millionths of a US dollar, when it reports one. Claude's
    /// is a client-side estimate (0004); Codex and Cursor report none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd_micros: Option<u64>,
}

impl Usage {
    /// Whether every count is zero and there is no cost.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        *self == Self::default()
    }

    /// What `self` added since `earlier`, for vendors that report running totals. A count that
    /// went down means the vendor started over, so all of `self` is new.
    #[must_use]
    pub fn since(&self, earlier: &Self) -> Self {
        fn delta(now: u64, before: u64) -> u64 {
            if now >= before { now - before } else { now }
        }
        Self {
            input_tokens: delta(self.input_tokens, earlier.input_tokens),
            output_tokens: delta(self.output_tokens, earlier.output_tokens),
            cache_read_tokens: delta(self.cache_read_tokens, earlier.cache_read_tokens),
            cache_write_tokens: delta(self.cache_write_tokens, earlier.cache_write_tokens),
            cost_usd_micros: match (self.cost_usd_micros, earlier.cost_usd_micros) {
                (Some(now), Some(before)) => Some(delta(now, before)),
                (now, _) => now,
            },
        }
    }
}

impl Add for Usage {
    type Output = Self;

    fn add(mut self, other: Self) -> Self {
        self += other;
        self
    }
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens = self.input_tokens.saturating_add(other.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cache_read_tokens = self
            .cache_read_tokens
            .saturating_add(other.cache_read_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(other.cache_write_tokens);
        self.cost_usd_micros = match (self.cost_usd_micros, other.cost_usd_micros) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            (a, b) => a.or(b),
        };
    }
}

/// The payload of [`Event::Usage`]: what one model used since the run's previous report.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDelta {
    /// The model, when the vendor breaks usage down by model, as Claude's `modelUsage` does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The new usage.
    #[serde(flatten)]
    pub usage: Usage,
}

/// Turns running totals into [`UsageDelta`]s, per model.
///
/// Codex's `turn.completed.usage` is cumulative for the thread, and Claude's `result.modelUsage`
/// includes earlier turns and carries over into resumed sessions (0004). A backend feeds each
/// total in and emits what comes out. For a resumed session, it starts from the totals the
/// previous run reached, with [`CumulativeUsage::with_baseline`].
#[derive(Clone, Debug, Default)]
pub struct CumulativeUsage {
    last: BTreeMap<Option<String>, Usage>,
}

impl CumulativeUsage {
    /// A meter that has seen nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A meter that treats `totals` as already reported.
    #[must_use]
    pub fn with_baseline(totals: impl IntoIterator<Item = (Option<String>, Usage)>) -> Self {
        Self {
            last: totals.into_iter().collect(),
        }
    }

    /// The delta since the last total for `model`, or `None` when nothing changed.
    pub fn observe(&mut self, model: Option<&str>, total: Usage) -> Option<UsageDelta> {
        let key = model.map(str::to_owned);
        let previous = self.last.get(&key).copied().unwrap_or_default();
        let usage = total.since(&previous);
        self.last.insert(key, total);
        (!usage.is_zero()).then(|| UsageDelta {
            model: model.map(str::to_owned),
            usage,
        })
    }
}

/// One limit window of an account, as a vendor last reported it: Claude's
/// `rate_limit_event.rate_limit_info`, or one of Codex's `primary` and `secondary` windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitWindow {
    /// The vendor's name for the window, such as `five_hour`, `seven_day`, `primary`, or
    /// `secondary`.
    pub window: String,
    /// The window's length in minutes, when the vendor says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<u32>,
    /// How much of the window is used, from 0 to 100, when the vendor says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    /// Whether the account may keep going.
    pub status: LimitStatus,
    /// When the window resets, when the vendor says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
}

/// Whether a limit window still allows requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LimitStatus {
    /// Requests go through.
    Allowed,
    /// Requests go through, but the window is nearly used up.
    Warning,
    /// Requests are refused until the window resets.
    Rejected,
    /// The vendor didn't say, or said something this version does not know.
    #[serde(other)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        CumulativeUsage, Event, ExitInfo, Failure, FailureKind, LimitStatus, LimitWindow, Outcome,
        Usage, UsageDelta,
    };

    fn tokens(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        }
    }

    #[test]
    fn events_serialize_with_a_kind_tag_and_camel_case_fields() {
        let event = Event::SessionStarted {
            session_id: "s-1".into(),
            model: Some("m".into()),
            api_key_source: None,
        };
        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            json!({"kind": "sessionStarted", "sessionId": "s-1", "model": "m"})
        );
        let usage = Event::Usage(UsageDelta {
            model: None,
            usage: Usage {
                cost_usd_micros: Some(1_500),
                ..tokens(3, 4)
            },
        });
        assert_eq!(
            serde_json::to_value(&usage).unwrap(),
            json!({
                "kind": "usage", "inputTokens": 3, "outputTokens": 4, "cacheReadTokens": 0,
                "cacheWriteTokens": 0, "costUsdMicros": 1500
            })
        );
        let failed = Event::Finished {
            outcome: Outcome::Failed(Failure {
                failure: FailureKind::RateLimited,
                message: "limit".into(),
                exit: Some(ExitInfo {
                    code: Some(1),
                    signal: None,
                }),
                stderr_tail: None,
            }),
        };
        let value = serde_json::to_value(&failed).unwrap();
        assert_eq!(
            value,
            json!({
                "kind": "finished",
                "outcome": {
                    "status": "failed", "failure": "rateLimited", "message": "limit",
                    "exit": {"code": 1}
                }
            })
        );
        assert_eq!(serde_json::from_value::<Event>(value).unwrap(), failed);
    }

    #[test]
    fn limit_windows_round_trip_with_rfc_3339_resets() {
        let window = Event::RateLimit(LimitWindow {
            window: "five_hour".into(),
            duration_minutes: Some(300),
            used_percent: Some(42.5),
            status: LimitStatus::Warning,
            resets_at: Some("2026-09-24T17:00:00Z".parse().unwrap()),
        });
        let value = serde_json::to_value(&window).unwrap();
        assert_eq!(value["resetsAt"], "2026-09-24T17:00:00Z");
        assert_eq!(serde_json::from_value::<Event>(value).unwrap(), window);
        let unknown: LimitWindow =
            serde_json::from_value(json!({"window": "w", "status": "throttled"})).unwrap();
        assert_eq!(unknown.status, LimitStatus::Unknown);
    }

    #[test]
    fn unknown_kinds_decode_to_their_fallbacks() {
        let failure: FailureKind = serde_json::from_value(json!("quotaExploded")).unwrap();
        assert_eq!(failure, FailureKind::Other);
    }

    #[test]
    fn usage_adds_field_by_field_and_keeps_a_cost_either_side_reported() {
        let a = Usage {
            cost_usd_micros: Some(10),
            ..tokens(1, 2)
        };
        let b = tokens(10, 20);
        assert_eq!(
            a + b,
            Usage {
                cost_usd_micros: Some(10),
                ..tokens(11, 22)
            }
        );
        assert_eq!(b + b, tokens(20, 40));
    }

    #[test]
    fn running_totals_become_deltas_per_model() {
        let mut meter = CumulativeUsage::new();
        assert_eq!(
            meter.observe(Some("opus"), tokens(100, 10)).unwrap().usage,
            tokens(100, 10)
        );
        assert_eq!(
            meter.observe(Some("opus"), tokens(150, 30)).unwrap().usage,
            tokens(50, 20)
        );
        let haiku = meter.observe(Some("haiku"), tokens(5, 1)).unwrap();
        assert_eq!(haiku.model.as_deref(), Some("haiku"));
        assert_eq!(haiku.usage, tokens(5, 1));
        assert_eq!(meter.observe(Some("opus"), tokens(150, 30)), None);
    }

    #[test]
    fn a_total_that_went_down_counts_as_new() {
        let mut meter = CumulativeUsage::new();
        meter.observe(None, tokens(100, 10));
        assert_eq!(
            meter.observe(None, tokens(7, 3)).unwrap().usage,
            tokens(7, 3)
        );
    }

    #[test]
    fn a_resumed_session_starts_from_its_baseline() {
        let mut meter = CumulativeUsage::with_baseline([(Some("opus".into()), tokens(100, 10))]);
        assert_eq!(
            meter.observe(Some("opus"), tokens(120, 15)).unwrap().usage,
            tokens(20, 5)
        );
    }
}
