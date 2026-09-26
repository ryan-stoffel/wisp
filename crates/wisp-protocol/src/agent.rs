//! Agent runs, behind the `agents` capability (0007, 0011, #156, decision 0014).
//!
//! `agent/start` starts a worker: a vendor CLI in its own git worktree of the project's
//! repository, with the worker sandbox (0013) and the project's shared context folder (0005).
//! `agent/send` messages it, `agent/cancel` stops it, `agent/list` lists runs, and `agent/events`
//! pages through one run's events from wispd's log. Everything a run does is also an `agent.*`
//! event on the project's `events/subscribe`.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::id::uuid_v7_id;
use crate::{AccountChoice, ProjectId, WispEvent};

uuid_v7_id! {
    /// An agent run's id: a version 7 UUID that the client generates once and sends again on every
    /// retry of `agent/start`, so a retry never starts a second agent.
    RunId
}

uuid_v7_id! {
    /// A follow-up turn's id: a version 7 UUID that the client generates once and sends again on
    /// every retry of `agent/send`, so a retry never sends the message twice.
    TurnId
}

/// What a run's tools may do. Only workers run through `agent/start`.
///
/// A newer wispd may send a policy this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentPolicy {
    /// Edits in the run's worktree and the shared context folder, commands in the vendor's OS
    /// sandbox (0004, 0013).
    WorkspaceWrite,
    /// A policy this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// Where a run is.
///
/// A newer wispd may send a status this version does not know; treat it as unknown, and don't
/// end a `switch` over this type in an exhaustiveness assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentStatus {
    /// wispd is creating its worktree or starting its CLI.
    Starting,
    /// Its CLI is running.
    Running,
    /// Its CLI finished its work, and wispd committed the changes.
    Completed,
    /// It failed; `error` says why.
    Failed,
    /// `agent/cancel` stopped it.
    Cancelled,
    /// wispd stopped while it ran. `agent/send` resumes it when it has a `sessionId`.
    Interrupted,
    /// `agent/accept` merged its changes into the project's branch and removed its worktree and
    /// branch. It takes no more messages.
    Accepted,
    /// A status this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// The commit wispd made for a run, compared with the commit its worktree was created from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    /// The commit's full sha, on the run's branch.
    pub commit: String,
    /// Files changed.
    pub files: u64,
    /// Lines added.
    pub insertions: u64,
    /// Lines removed.
    pub deletions: u64,
}

/// One agent run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentRun {
    /// The run's id.
    pub id: RunId,
    /// Its project.
    pub project: ProjectId,
    /// The task it was started with.
    pub prompt: String,
    /// What its tools may do.
    pub policy: AgentPolicy,
    /// Where it is.
    pub status: AgentStatus,
    /// The backend running it, such as `claude`.
    pub backend: String,
    /// The account it is charged to now: the backend's name for a subscription, or a key
    /// account's id. It changes on `agent.accountFallback`.
    pub account_id: String,
    /// Its worktree's branch, such as `wisp/1a2b3c4d`, once the worktree exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch: Option<String>,
    /// Its worktree's absolute path on the host, once it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub worktree_path: Option<String>,
    /// The vendor's session id, once the CLI reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
    /// Why it failed, for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    /// Its latest commit, once wispd made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diff: Option<DiffSummary>,
    /// When it was created, in RFC 3339 UTC.
    pub created_at: Timestamp,
    /// When it last changed, in RFC 3339 UTC.
    pub updated_at: Timestamp,
}

/// The part of a run that changes while it runs, as `agent.updated` reports it. The rest of
/// [`AgentRun`], including its prompt, never changes after `agent.started`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunState {
    /// Where the run is.
    pub status: AgentStatus,
    /// The account it is charged to now.
    pub account_id: String,
    /// The vendor's session id, once the CLI reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
    /// Why it failed, for people. Absent once it runs again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    /// Its latest commit, once wispd made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diff: Option<DiffSummary>,
    /// When it changed, in RFC 3339 UTC.
    pub updated_at: Timestamp,
}

/// Why a run failed, for code to match on.
///
/// A newer wispd may send a kind this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentFailureKind {
    /// The CLI isn't signed in, or its login expired.
    NotSignedIn,
    /// The account hit a usage limit.
    RateLimited,
    /// The run broke its tool policy or its sandbox's contract.
    PolicyViolation,
    /// The CLI used credentials or a provider other than the account's.
    UnexpectedApiKey,
    /// The CLI reported an error of its own.
    VendorError,
    /// The CLI died or exited unsuccessfully without saying why.
    Crashed,
    /// The CLI could not be started.
    SpawnFailed,
    /// wispd could not commit the run's changes, for example because the repository has no git
    /// identity.
    CommitFailed,
    /// A fault in wispd.
    Internal,
    /// A kind this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// How one CLI process of a run ended.
///
/// A newer wispd may send a status this version does not know; treat it as unknown.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AgentOutcome {
    /// The CLI finished its work.
    Completed {
        /// The last turn's final text, when the vendor reports one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        result: Option<String>,
    },
    /// `agent/cancel` stopped it.
    Cancelled,
    /// It failed.
    Failed {
        /// What went wrong.
        failure: AgentFailureKind,
        /// A description for people.
        message: String,
    },
    /// wispd stopped while it ran.
    Interrupted,
    /// A status this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// How a tool call ended.
///
/// A newer wispd may send a status this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentToolStatus {
    /// It ran and succeeded.
    Ok,
    /// It ran and failed.
    Error,
    /// The run's policy or sandbox refused it.
    Denied,
    /// A status this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// How far along a checklist item is.
///
/// A newer wispd may send a status this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentTodoStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    InProgress,
    /// Done.
    Completed,
    /// A status this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// One item of an agent's checklist.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentTodoItem {
    /// What to do.
    pub text: String,
    /// How far along it is.
    pub status: AgentTodoStatus,
}

/// One thing in a run's transcript: a normalized event from its CLI (0004), by `kind`.
///
/// A newer wispd may send kinds that are not listed here; skip them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AgentOutputItem {
    /// The CLI started or resumed its session.
    SessionStarted {
        /// The vendor's session id.
        session_id: String,
        /// The model, when the CLI says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        model: Option<String>,
    },
    /// The CLI took a turn's message: the prompt, or a follow-up.
    TurnStarted {
        /// The turn's id, when the caller gave one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        turn_id: Option<TurnId>,
    },
    /// Part of the assistant's reply, as it streams.
    TextDelta {
        /// The vendor's id for the message, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        message_id: Option<String>,
        /// The new text.
        text: String,
    },
    /// One whole assistant message.
    Text {
        /// The vendor's id for the message, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        message_id: Option<String>,
        /// The text.
        text: String,
    },
    /// The agent called a tool.
    ToolCall {
        /// The call's id, which its `toolResult` repeats.
        call_id: String,
        /// The tool, in the vendor's naming, such as `Edit`.
        name: String,
        /// The tool's input as the vendor sent it, or `{"truncated": true, "bytes": n}` when it
        /// was too large to forward.
        input: Value,
    },
    /// A tool call ended.
    ToolResult {
        /// The call's id.
        call_id: String,
        /// How it ended.
        status: AgentToolStatus,
        /// What the tool returned, when the vendor includes it, cut short when it is long.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        output: Option<String>,
    },
    /// The model's reasoning, when the vendor shows it.
    Reasoning {
        /// The vendor's id for the message, when it has one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        message_id: Option<String>,
        /// The text.
        text: String,
    },
    /// The agent's current checklist, replacing the previous one.
    TodoList {
        /// The items, in order.
        items: Vec<AgentTodoItem>,
    },
    /// A message from the vendor for the user that doesn't end the run.
    Notice {
        /// The message.
        detail: String,
    },
    /// A turn ended.
    TurnFinished {
        /// The turn's id, as its `turnStarted` had it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        turn_id: Option<TurnId>,
        /// The turn's final text, when the vendor reports one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        result: Option<String>,
    },
    /// A follow-up never reached the CLI, because the run ended first. Send it again.
    FollowUpDropped {
        /// The follow-up's turn id.
        turn_id: TurnId,
    },
    /// Tokens and cost used since the previous `usage` item.
    Usage {
        /// The model, when the vendor breaks usage down by model.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        model: Option<String>,
        /// Input tokens, not counting cache reads and writes.
        input_tokens: u64,
        /// Output tokens.
        output_tokens: u64,
        /// Input tokens read from the prompt cache.
        cache_read_tokens: u64,
        /// Input tokens written to the prompt cache.
        cache_write_tokens: u64,
        /// The cost the vendor reported, in millionths of a US dollar, when it reports one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        cost_usd_micros: Option<u64>,
    },
    /// Something in the CLI's output that wispd skipped.
    Warning {
        /// A short description.
        detail: String,
    },
    /// A kind this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// Params of `agent/start`.
///
/// Idempotent on `runId`: starting the same id again with the same params returns the run
/// instead of starting another; with different params it fails with `idConflict`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentStartParams {
    /// The new run's id, a version 7 UUID generated by the client.
    pub run_id: RunId,
    /// The project whose repository the run works in.
    pub project: ProjectId,
    /// The task.
    pub prompt: String,
    /// What the run's tools may do. Only `workspaceWrite` exists.
    pub policy: AgentPolicy,
    /// The account to run on. Absent means the worker role's default (`accounts/defaults/*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<AccountChoice>,
}

/// Result of `agent/start`, `agent/send`, and `agent/cancel`: the run as it stands.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunResult {
    /// The run.
    pub run: AgentRun,
}

/// Params of `agent/send`: a message to a run (0011).
///
/// A running agent gets it as its next turn. An agent that ended with a `sessionId` resumes
/// that session in its worktree, with the message as the prompt. Idempotent on `turnId` while
/// wispd runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentSendParams {
    /// The run.
    pub run_id: RunId,
    /// The message's id, a version 7 UUID generated by the client.
    pub turn_id: TurnId,
    /// The message.
    pub text: String,
}

/// Params of `agent/cancel`: stops a running agent, which ends as `cancelled`. Cancelling a run
/// that isn't running does nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentCancelParams {
    /// The run.
    pub run_id: RunId,
}

/// Params of `agent/list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentListParams {
    /// Only this project's runs. Absent lists every run on the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project: Option<ProjectId>,
}

/// Result of `agent/list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentListResult {
    /// The runs, oldest first.
    pub runs: Vec<AgentRun>,
    /// The `seq` of the last event the list reflects, to subscribe after.
    pub seq: u64,
}

/// Params of `agent/events`: one run's events from wispd's log, for rebuilding its transcript
/// after `resyncRequired` or a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventsParams {
    /// The run.
    pub run_id: RunId,
    /// Return the events whose `seq` is greater than this; 0 for the first page.
    pub after: u64,
    /// The most events to return: 500 by default, and at most 1000.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u32>,
}

/// Result of `agent/events`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventsResult {
    /// The events, oldest first.
    pub events: Vec<LoggedEvent>,
    /// Whether more events follow the last one returned. Ask again after its `seq`.
    pub more: bool,
}

/// An event from wispd's log, as `agent/events` returns it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct LoggedEvent {
    /// The event's position in the log.
    pub seq: u64,
    /// When it happened, in RFC 3339 UTC.
    pub time: Timestamp,
    /// Its project. Absent for host-level events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project: Option<ProjectId>,
    /// What happened.
    pub event: WispEvent,
}
