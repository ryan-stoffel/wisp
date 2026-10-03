//! Agent runs, behind the `agents` capability (0007, 0011, #156, decision 0014).
//!
//! `agent/start` starts a worker: a vendor CLI in its own git worktree of the project's
//! repository, with the worker sandbox (0013) and the project's shared context folder (0005).
//! `agent/send` messages it, `agent/cancel` stops it, `agent/list` lists runs, and `agent/events`
//! pages through one run's events from plxd's log. Everything a run does is also an `agent.*`
//! event on the project's `events/subscribe`.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::id::uuid_v7_id;
use crate::{
    AccountChoice, AgentApprovalBy, AgentApprovalDecision, ApprovalId, ParallaxEvent, ProjectId,
};

uuid_v7_id! {
    /// An agent run's id: a version 7 UUID that the client generates once and sends again on every
    /// retry of `agent/start`, so a retry never starts a second agent.
    RunId
}

uuid_v7_id! {
    /// A project's coordinator thread (M4, #195, decision 0019). Runs the coordinator starts
    /// through its Parallax tools carry it, so a client can tell them from runs it started itself.
    CoordinatorThreadId
}

uuid_v7_id! {
    /// A follow-up turn's id: a version 7 UUID that the client generates once and sends again on
    /// every retry of `agent/send`, so a retry never sends the message twice.
    TurnId
}

uuid_v7_id! {
    /// A stored image's id (RYA-191, decision 0026): a version 7 UUID that plxd generates once a
    /// message's image reaches the CLI. `turnStarted` lists them, and `agent/image` serves them.
    ImageId
}

/// An image's file type (RYA-191): the four that Claude and Codex both take.
///
/// A newer peer may send a type this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
pub enum ImageMediaType {
    /// PNG.
    #[serde(rename = "image/png")]
    Png,
    /// JPEG.
    #[serde(rename = "image/jpeg")]
    Jpeg,
    /// GIF.
    #[serde(rename = "image/gif")]
    Gif,
    /// WebP.
    #[serde(rename = "image/webp")]
    Webp,
    /// A type this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// An image sent with a prompt or message, behind the `promptImages` capability (RYA-191,
/// decision 0026). The CLI gets it beside the text, never as a file name or path in it. A
/// project's or repo's icon image has the same shape (0038).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct PromptImage {
    /// Its file type, which its bytes must match.
    pub media_type: ImageMediaType,
    /// The image file's bytes, in standard base64 with padding.
    pub data: String,
}

/// What a run's tools may do. `agent/start` takes only `workspaceWrite`; a project's coordinator,
/// which `project/start` starts, is `noWrite`.
///
/// A newer plxd may send a policy this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentPolicy {
    /// Edits in the run's worktree and the shared context folder, commands in the vendor's OS
    /// sandbox (0004, 0013).
    WorkspaceWrite,
    /// A project's coordinator (0024): full Claude Code in its permission mode, in the project's
    /// repository, plus plxd's coordinator tools (0019, 0027). Without those tools, read-only
    /// tools (0004).
    NoWrite,
    /// A policy this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// How hard a run's model thinks, behind the `runOptions` capability (RYA-97). Claude Code takes
/// every level as `--effort`, and downgrades `xhigh` on models that lack it. A backend that can't
/// honor a level refuses the run with `unsupportedOption`.
///
/// A newer peer may send a level this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentEffort {
    /// The least thinking.
    Low,
    /// Some thinking.
    Medium,
    /// More thinking.
    High,
    /// More than `high`, on models that offer it.
    Xhigh,
    /// The most thinking.
    Max,
    /// A level this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// A run's permission mode, behind the `runOptions` capability (RYA-97): Claude Code's modes,
/// which each backend reports the subset of that it maps (RYA-188, 0027). A worker keeps the
/// worker sandbox (0013) in every mode but [`AgentPermission::Bypass`].
///
/// A newer peer may send a value this version does not know; treat it as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentPermission {
    /// A classifier approves or blocks each action instead of a prompt: Claude Code's `auto`.
    Auto,
    /// Asks before each action that needs approval: Claude Code's `default`. Headless, a
    /// request nobody can answer is denied.
    Manual,
    /// Edits files and runs commands without asking: the default. Claude Code's `acceptEdits`.
    Edit,
    /// Reads and plans without editing: Claude Code's plan mode, whose file tools refuse to
    /// write.
    Plan,
    /// Skips every permission check: Claude Code's `bypassPermissions`. A worker in this mode
    /// runs without the worker sandbox, as Claude Code does on the user's own machine.
    Bypass,
    /// A value this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// Where a run is.
///
/// A newer plxd may send a status this version does not know; treat it as unknown, and don't
/// end a `switch` over this type in an exhaustiveness assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AgentStatus {
    /// plxd is creating its worktree or starting its CLI.
    Starting,
    /// Its CLI is running.
    Running,
    /// Its CLI finished its work, and plxd committed a worker's changes.
    Completed,
    /// It failed; `error` says why.
    Failed,
    /// `agent/cancel` stopped it.
    Cancelled,
    /// plxd stopped while it ran. `agent/send` resumes it when it has a `sessionId`.
    Interrupted,
    /// A usage limit stopped it, and plxd resumes it at `resumeAt` (PLX-371, decision 0049).
    /// `agent/send` and `agent/resumeNow` resume it sooner, and `agent/cancel` makes it
    /// `cancelled`.
    Waiting,
    /// `agent/accept` merged its changes into the project's branch and removed its worktree and
    /// branch. It takes no more messages.
    Accepted,
    /// A status this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// The commit plxd made for a run, compared with the commit its worktree was created from.
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
    /// Its worktree's branch, such as `parallax/1a2b3c4d`, once the worktree exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch: Option<String>,
    /// Its worktree's absolute path on the host, once it exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub worktree_path: Option<String>,
    /// True when the worktree's base was resolved from a dirty `HEAD` (#257): the repository's
    /// tracked files had uncommitted changes that aren't in this run. Never true for a run whose
    /// worktree doesn't exist yet, or whose caller passed an explicit base.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub base_dirty: bool,
    /// The vendor's session id, once the CLI reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
    /// Why it failed, for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    /// Its latest commit, once plxd made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diff: Option<DiffSummary>,
    /// The coordinator thread that started it through its Parallax tools. Absent for a run a client
    /// started itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coordinator_thread: Option<CoordinatorThreadId>,
    /// Its model: what it was started with, or what `agent/send` last changed it to. Absent
    /// means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// Its effort, as `model`. Absent means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<AgentEffort>,
    /// Its context window in tokens, as `model`. Absent means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_window: Option<u32>,
    /// Whether it runs in fast mode, as `model`. Absent means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fast: Option<bool>,
    /// Its permission, as `model`. Absent means `edit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<AgentPermission>,
    /// True when it forwards its permission requests to the client, as the start method that
    /// made it asked with `approvals` (RYA-222, decision 0031). It never changes. Absent means
    /// false: its CLI denies what would prompt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approvals: bool,
    /// True for a thread started with `checkout`: it works in its repository's own checkout, so it
    /// has no `branch`, `worktreePath`, or `diff`, and its changes are left uncommitted there.
    /// Absent means false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checkout: bool,
    /// The web URLs of the pull requests linked to it, oldest first, with no duplicates: the one
    /// `agent/openPr` returned, and any its agent opened with `gh pr create` (PLX-318). Behind the
    /// `pullRequests` capability. Absent means none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<String>,
    /// When plxd resumes it, while it is `waiting` (decision 0049). Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resume_at: Option<Timestamp>,
    /// Whether a usage limit makes it wait and resume, overriding the host's
    /// `host/settings` `autoResume` (decision 0049). Absent means the host's setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_resume: Option<bool>,
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
    /// The backend it runs on, which `agent/send`'s `account` can move it to. Absent from a plxd
    /// without `sendAccount`, whose runs never move.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub backend: Option<String>,
    /// The vendor's session id, once the CLI reported it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_id: Option<String>,
    /// Why it failed, for people. Absent once it runs again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
    /// Its latest commit, once plxd made one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diff: Option<DiffSummary>,
    /// Its model, which `agent/send` can change (RYA-163). Absent means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// Its effort, which `agent/send` can change (RYA-161). Absent means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<AgentEffort>,
    /// Its context window in tokens, which `agent/send` can change. Absent means the CLI's
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_window: Option<u32>,
    /// Whether it runs in fast mode, which `agent/send` can change. Absent means the CLI's
    /// default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fast: Option<bool>,
    /// Its permission, which `agent/send` can change (RYA-161). Absent means `edit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<AgentPermission>,
    /// Its linked pull requests, as `AgentRun.pullRequests` (PLX-318). Absent means none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pull_requests: Vec<String>,
    /// When plxd resumes it, as `AgentRun.resumeAt`. Absent once it doesn't wait.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub resume_at: Option<Timestamp>,
    /// Its auto-resume override, as `AgentRun.autoResume`. Absent means the host's setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_resume: Option<bool>,
    /// When it changed, in RFC 3339 UTC.
    pub updated_at: Timestamp,
}

/// Why a run failed, for code to match on.
///
/// A newer plxd may send a kind this version does not know; treat it as unknown.
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
    /// plxd could not commit the run's changes, for example because the repository has no git
    /// identity.
    CommitFailed,
    /// A fault in plxd.
    Internal,
    /// A kind this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// How one CLI process of a run ended.
///
/// A newer plxd may send a status this version does not know; treat it as unknown.
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
    /// plxd stopped while it ran.
    Interrupted,
    /// A status this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// How a tool call ended.
///
/// A newer plxd may send a status this version does not know; treat it as unknown.
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
/// A newer plxd may send a status this version does not know; treat it as unknown.
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
/// A newer plxd may send kinds that are not listed here; skip them.
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
        /// A follow-up's message, as `agent/send` took it, cut short when it is long. Absent for
        /// the prompt's turn, whose text is the run's `prompt`, and in logs from before plxd
        /// recorded it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        text: Option<String>,
        /// True for a wake-up (RYA-42, decision 0025): a turn plxd sent a project's coordinator
        /// on its own, not the user, because runs it started finished. `text` lists them. Also
        /// true for the turn plxd sends a run once its usage limit resets (PLX-371, decision
        /// 0049).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        wake: bool,
        /// The thread that sent a follow-up through its Parallax tools (0041), as `agent/send`'s
        /// `from` named it. Absent for the user's own message and for the prompt's turn, whose
        /// sender is the thread's `parent`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        from: Option<RunId>,
        /// The images sent with the turn's message, the prompt's or a follow-up's, in order, for
        /// `agent/image` (RYA-191). Absent when it had none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<ImageId>,
        /// The threads attached to the turn's message as context, in order (PLX-372). The agent
        /// got a summary of each ahead of the message, which `text` and the run's `prompt` leave
        /// out. Absent when it had none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        threads: Vec<RunId>,
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
    /// Another thread stopped the run through its Parallax tools, with `agent/cancel`'s `from`
    /// (0041). The run's `agent.finished` follows.
    Interrupted {
        /// The thread that stopped it.
        from: RunId,
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
    /// Something in the CLI's output that plxd skipped.
    Warning {
        /// A short description.
        detail: String,
    },
    /// The agent asks to use a tool and waits for `agent/approve` (RYA-222, decision 0031), in a
    /// run started with `approvals`. It is pending until its `approvalResolved`, or until the
    /// run's next `agent.finished`.
    ApprovalRequested {
        /// The request's id.
        approval_id: ApprovalId,
        /// The tool, in the vendor's naming, such as `Bash` or `ExitPlanMode`.
        tool_name: String,
        /// The tool's input as the vendor sent it, such as `ExitPlanMode`'s `plan`, or
        /// `{"truncated": true, "bytes": n}` when it was too large to forward.
        input: Value,
        /// The tool call's id, which its `toolCall` and `toolResult` carry, when the vendor
        /// says.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        call_id: Option<String>,
        /// Why the CLI asks, such as a safety check's warning, with terminal escapes removed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reason: Option<String>,
        /// The path that made the CLI ask, when one did.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        blocked_path: Option<String>,
        /// The vendor's id for the subagent asking, when one of the agent's own subagents asks.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        subagent: Option<String>,
        /// The rules that `always` adds for the rest of the CLI process, such as
        /// `Bash(pnpm test:*)`. Absent when the request offers none.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        always_allow: Vec<String>,
        /// True when the request is a question for the user rather than one action to allow,
        /// such as `ExitPlanMode`'s plan: show its input in full.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        interactive: bool,
        /// When plxd denies it if nobody has answered, in RFC 3339 UTC.
        expires_at: Timestamp,
    },
    /// How a permission request ended (RYA-222, decision 0031).
    ApprovalResolved {
        /// The request's id.
        approval_id: ApprovalId,
        /// What it came to.
        decision: AgentApprovalDecision,
        /// Who or what decided it.
        by: AgentApprovalBy,
        /// True when it was allowed for the rest of the CLI process as well.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        always: bool,
        /// The user's message to the agent with a denial, cut short when it is long.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        message: Option<String>,
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
    /// What the run's tools may do: only `workspaceWrite`. A project's coordinator, the one
    /// `noWrite` run, is started with `project/start`.
    pub policy: AgentPolicy,
    /// The account to run on. Absent means the worker role's default (`accounts/defaults/*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<AccountChoice>,
    /// The coordinator thread starting the run, which `plxd mcp` sets for runs the coordinator
    /// spawns (0019). The run keeps it, and a retry must repeat it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coordinator_thread: Option<CoordinatorThreadId>,
    /// The model, in the backend's naming, such as `opus`. Absent means the CLI's default. Send
    /// it, `effort`, and `permission` only to a plxd that advertises `runOptions`. The run keeps
    /// all three when it resumes, and a retry must repeat them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// How hard the model thinks. Absent means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<AgentEffort>,
    /// The context window in tokens, one the backend offers: Claude Code's `200000` or
    /// `1000000`, Codex's `272000` or `872000`. Absent means the CLI's default. Send it and
    /// `fast` only to a plxd that advertises `contextAndFast`. The run keeps both when it
    /// resumes, and a retry must repeat them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_window: Option<u32>,
    /// Fast mode on or off: Claude Code's fast mode, or Codex's priority service tier. Absent
    /// means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fast: Option<bool>,
    /// The permission mode (RYA-97, 0027). Absent means `edit`, or for a run with a
    /// `coordinatorThread`, the coordinator's mode when it spawns the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<AgentPermission>,
    /// Images for the prompt, sent only to a plxd that advertises `promptImages`. Its options
    /// give the caps: `maxImages`, and `maxImageBytes` and `maxTotalBytes` of `data`, past which
    /// the request fails with `imageTooLarge`. With images, the prompt may be empty (RYA-193). A
    /// retry must repeat them; plxd doesn't compare them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<PromptImage>,
    /// Forward the run's permission requests to the client as `approvalRequested` items, which
    /// `agent/approve` answers (RYA-222, decision 0031). Set it only when the client shows and
    /// answers them, and only to a plxd that advertises `approvals`. A thread with it is full
    /// Claude Code and also asks in Accept Edits (0034). Absent, a run in Manual, Auto, or Plan
    /// denies what would prompt, and a thread keeps the worker sandbox, as before. A run with a
    /// `coordinatorThread` also gets it when its coordinator has it. The run keeps it when it
    /// resumes, and a retry must repeat it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approvals: bool,
    /// Threads attached to the prompt as context, by their run ids, sent only to a plxd that
    /// advertises `threadContext` (PLX-372, decision 0047). The agent gets a summary of each ahead
    /// of the prompt: its id and what was said in it, without tool calls, cut from the front to
    /// the capability's `maxSummaryBytes`. At most the capability's `maxThreads`. An id that is
    /// no thread's fails with `threadNotFound`. A retry must repeat them; plxd doesn't compare
    /// them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<RunId>,
}

/// Result of `agent/start`, `agent/send`, `agent/cancel`, `agent/resumeNow`, and
/// `agent/autoResume`: the run as it stands.
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
/// plxd runs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentSendParams {
    /// The run.
    pub run_id: RunId,
    /// The message's id, a version 7 UUID generated by the client.
    pub turn_id: TurnId,
    /// The message.
    pub text: String,
    /// A new model for the run and every later resume (RYA-163), sent only to a plxd that
    /// advertises `sendModel`. It should be one the run's backend runs, or with `account`, that
    /// account's; plxd can't check that, so another's fails the run with the CLI's own error.
    /// Absent, or the run's own, changes nothing. While the run's CLI is running, a different one
    /// waits with the message until the CLI exits, since it can't change mid-process, as does
    /// every message sent after it; a plxd without `sendAccount` fails it with
    /// `unsupportedOption` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// A new effort (RYA-161), as `model`. `sendOptions` is enough for it and `permission`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<AgentEffort>,
    /// A new permission (RYA-161), as `effort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<AgentPermission>,
    /// A new context window, as `effort`, but sent only to a plxd that advertises
    /// `contextAndFast`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_window: Option<u32>,
    /// Fast mode on or off, as `contextWindow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fast: Option<bool>,
    /// A new account for the run and every later resume, sent only to a plxd that advertises
    /// `sendAccount`, and waiting for a running CLI as `model` does. On the run's backend, the
    /// session resumes on it. On another backend, the session can't move, so plxd starts a new
    /// one there in the run's worktree, whose first message carries the conversation so far
    /// before this one: the run keeps its id, transcript, and worktree, and takes the new
    /// backend, with `model`, and the run's effort, permission, context window, and fast mode where
    /// the backend maps them.
    /// Absent, or the run's own, changes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<AccountChoice>,
    /// Images for the message, as `agent/start`'s.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<PromptImage>,
    /// Threads attached to the message as context, as `agent/start`'s. A message that waits for
    /// the run's CLI gets their summaries when it's sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<RunId>,
    /// The thread sending it through its Parallax tools (0041), which the turn's `turnStarted`
    /// names. It must be a run on the host, or the send fails with `runNotFound`. Absent for the
    /// user's own message. Behind `threadTools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from: Option<RunId>,
}

/// Params of `agent/cancel`: stops a running agent, which ends as `cancelled`. Cancelling a run
/// that isn't running does nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentCancelParams {
    /// The run.
    pub run_id: RunId,
    /// The thread stopping it through its Parallax tools (0041): a running run logs an
    /// `interrupted` item naming it. It must be a run on the host, or the cancel fails with
    /// `runNotFound`. Behind `threadTools`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from: Option<RunId>,
}

/// Params of `agent/resumeNow`: resumes a `waiting` run now instead of at its `resumeAt`, with
/// the same message the timer sends (decision 0049). Fails with `runNotResumable` for a run that
/// isn't waiting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentResumeNowParams {
    /// The run.
    pub run_id: RunId,
}

/// Params of `agent/autoResume`: sets or clears a run's auto-resume override (decision 0049).
/// Turning it off for a `waiting` run clears its timer, and the run becomes `failed` with its
/// usage limit's error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentAutoResumeParams {
    /// The run.
    pub run_id: RunId,
    /// On or off for this run. Absent clears the override, so the host's setting applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auto_resume: Option<bool>,
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

/// Params of `agent/events`: one run's events from plxd's log, for rebuilding its transcript
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

/// Params of `agent/image`: one image sent with a run's messages, by an id from its
/// `turnStarted` (RYA-191). Its result is the [`PromptImage`] as it was sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AgentImageParams {
    /// The run.
    pub run_id: RunId,
    /// The image.
    pub image_id: ImageId,
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

/// An event from plxd's log, as `agent/events` returns it.
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
    pub event: ParallaxEvent,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AgentOutcome, AgentOutputItem, AgentStatus};

    #[test]
    fn unknown_statuses_kinds_and_outcomes_decode_as_unknown() {
        assert_eq!(
            serde_json::from_value::<AgentStatus>(json!("paused")).unwrap(),
            AgentStatus::Unknown
        );
        assert_eq!(
            serde_json::from_value::<AgentOutputItem>(json!({"kind": "image", "url": "x"}))
                .unwrap(),
            AgentOutputItem::Unknown
        );
        assert_eq!(
            serde_json::from_value::<AgentOutcome>(json!({"status": "merged"})).unwrap(),
            AgentOutcome::Unknown
        );
    }

    #[test]
    fn outcomes_tag_by_status() {
        assert_eq!(
            serde_json::to_value(AgentOutcome::Cancelled).unwrap(),
            json!({"status": "cancelled"})
        );
        assert_eq!(
            serde_json::to_value(AgentOutcome::Completed { result: None }).unwrap(),
            json!({"status": "completed"})
        );
    }
}
