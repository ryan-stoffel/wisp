use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::uuid_v7_id;
use crate::{
    AgentFailureKind, AgentMerge, AgentOutcome, AgentOutputItem, AgentRun, AgentRunState,
    ContextFile, DiffSummary, Project, ProjectId, Repo, RepoId, RunId, Thread,
};

uuid_v7_id! {
    /// Identifies one `events/subscribe` on one connection. wispd generates it.
    SubscriptionId
}

uuid_v7_id! {
    /// Identifies wispd's event log. It changes only when the log starts over.
    LogId
}

/// Params of `events/subscribe`.
///
/// wispd replays the events after `after`, then sends new ones as they happen, each as an
/// `events/event` notification. If those events are gone or too many to replay, it fails with
/// `resyncRequired`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EventsSubscribeParams {
    /// Replay the events whose `seq` is greater than this: usually the `seq` of a snapshot, such
    /// as `project/list`'s, or of the last event received. `seq` starts at 1.
    pub after: u64,
    /// Subscribe to one project's events. Without it, the subscription gets host-level events,
    /// such as `project.created`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project: Option<ProjectId>,
}

/// Result of `events/subscribe`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EventsSubscribeResult {
    /// The new subscription, which its `events/event` notifications name.
    pub subscription: SubscriptionId,
}

/// Params of `events/unsubscribe`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EventsUnsubscribeParams {
    /// The subscription to end. Ending one that does not exist succeeds.
    pub subscription: SubscriptionId,
}

/// Result of `events/unsubscribe`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EventsUnsubscribeResult {}

/// Params of `events/event`: one event from wispd's event log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct EventsEventParams {
    /// The subscription it belongs to.
    pub subscription: SubscriptionId,
    /// The event's position in wispd's log. It increases by one for every change on the host,
    /// across all projects.
    pub seq: u64,
    /// When the change happened, in RFC 3339 UTC.
    pub time: Timestamp,
    /// The project it belongs to. Absent for host-level events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project: Option<ProjectId>,
    /// What happened.
    pub event: WispEvent,
}

/// What happened, by `kind`.
///
/// A newer wispd may send kinds that are not listed here. Skip those events but still count their
/// `seq` as received, and don't end a `switch` over this type in an exhaustiveness assertion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum WispEvent {
    /// A project was created. Host-level.
    #[serde(rename = "project.created")]
    ProjectCreated {
        /// The new project.
        project: Project,
    },
    /// A shared context file was created or changed (0005, #155): from `context/write`, or from
    /// an agent's own write, detected on disk. Project-scoped.
    #[serde(rename = "context.changed")]
    ContextChanged {
        /// The changed file.
        file: ContextFile,
    },
    /// An agent run was created (#156). Project-scoped, like every `agent.*` event.
    #[serde(rename = "agent.started")]
    AgentStarted {
        /// The run's id.
        run_id: RunId,
        /// The run as it was created. wispd always sends it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        run: Option<AgentRun>,
    },
    /// Something about a run changed: its status, its session, its account, or its latest diff.
    /// It carries only the part of the run that changes; apply it to the run from
    /// `agent.started` or `agent/list`.
    #[serde(rename = "agent.updated")]
    AgentUpdated {
        /// The run's id.
        run_id: RunId,
        /// The run's changing fields as they stand now.
        state: AgentRunState,
    },
    /// Part of a run's transcript. wispd sends at most one per run every 50 ms, with everything
    /// that happened in between.
    #[serde(rename = "agent.output")]
    AgentOutput {
        /// The run's id.
        run_id: RunId,
        /// What happened, in order.
        items: Vec<AgentOutputItem>,
    },
    /// Routing (#119) moved a run to another account after its subscription failed signed out
    /// or rate limited. Its usage from here on is charged to `toAccount`.
    #[serde(rename = "agent.accountFallback")]
    AgentAccountFallback {
        /// The run's id.
        run_id: RunId,
        /// The account it was on.
        from_account: String,
        /// The account it is on now.
        to_account: String,
        /// Why it moved.
        reason: AgentFailureKind,
    },
    /// A run's CLI process ended. A later `agent/send` can start another in the same run.
    #[serde(rename = "agent.finished")]
    AgentFinished {
        /// The run's id.
        run_id: RunId,
        /// How it ended.
        outcome: AgentOutcome,
    },
    /// wispd committed a run's changes on its branch.
    #[serde(rename = "agent.diffReady")]
    AgentDiffReady {
        /// The run's id.
        run_id: RunId,
        /// The commit and its stats against the worktree's base.
        diff: DiffSummary,
    },
    /// `agent/accept` merged a run's commit into the project's branch on the host (#157). The
    /// run's worktree and branch are gone; an `agent.updated` with status `accepted` follows.
    #[serde(rename = "agent.accepted")]
    AgentAccepted {
        /// The run's id.
        run_id: RunId,
        /// What happened to the project's repository.
        merge: AgentMerge,
    },
    /// `repo/add` registered a repository for normal threads, or wispd made its scratch entry
    /// (#110). Host-level.
    #[serde(rename = "repo.added")]
    RepoAdded {
        /// The entry.
        repo: Repo,
    },
    /// `thread/start` started a normal thread (#110). Host-level; its run's `agent.*` events go
    /// to its repo entry's id.
    #[serde(rename = "thread.started")]
    ThreadStarted {
        /// The thread.
        thread: Thread,
    },
    /// A thread was archived or brought back (#110). Host-level.
    #[serde(rename = "thread.updated")]
    ThreadUpdated {
        /// The thread as it stands.
        thread: Thread,
    },
    /// `thread/delete` deleted a thread and its run (#110). Host-level.
    #[serde(rename = "thread.deleted")]
    ThreadDeleted {
        /// The thread's run id.
        run_id: RunId,
        /// Its repo entry.
        repo: RepoId,
    },
    /// A kind this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}
