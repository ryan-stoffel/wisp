//! Normal threads, behind the `threads` capability (0011, #110, decision 0017).
//!
//! A normal thread is one agent with no coordinator: an agent run (0014) that belongs to a repo
//! entry instead of a project. A repo entry is a lightweight record of a repository on the host,
//! registered by path with `repo/add`. A thread with no repo, a quick chat, runs in a scratch
//! repository plxd makes for it, and belongs to plxd's scratch entry.
//!
//! A thread's run is an ordinary [`AgentRun`] whose `project` is its repo entry's id, so
//! `agent/list`, `agent/send`, `agent/cancel`, `agent/events`, `events/subscribe`, and every
//! `agent.*` event take a repo entry's id as they take a project's. What threads add is the repo
//! entries, the archived flag, and host-level `repo.*` and `thread.*` events.
//!
//! Since 0041, behind `threadLineage`, a thread also has a parent (the run that launched it), a
//! fork origin, a title, and a settled flag.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::uuid_v7_id;
use crate::{
    AccountChoice, AgentEffort, AgentPermission, AgentRun, ProjectIcon, PromptImage, RunId, TurnId,
};

/// The longest title `thread/start` and `thread/update` take, in bytes once trimmed (0041).
pub const MAX_THREAD_TITLE_BYTES: usize = 256;

uuid_v7_id! {
    /// A repo entry's id: a version 7 UUID that the client generates once and sends again on
    /// every retry of `repo/add`. plxd generates the scratch entry's.
    RepoId
}

/// A repository on the host that normal threads run in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Repo {
    /// The entry's id.
    pub id: RepoId,
    /// The name shown in the app: the repository folder's name.
    pub name: String,
    /// The absolute, canonical path of the repository on the host.
    pub path: String,
    /// True for plxd's scratch entry, which holds the threads with no repo. Its `path` is the
    /// folder that holds each such thread's own scratch repository.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub scratch: bool,
    /// The icon the user chose, in a project's shape (0032), behind the `threadAttention`
    /// capability (0033). Absent means the app's default: the name's initials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub icon: Option<ProjectIcon>,
    /// When the entry was created, in RFC 3339 UTC.
    pub created_at: Timestamp,
}

/// A normal thread: what threads add to its run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    /// The thread's run id.
    pub id: RunId,
    /// Its repo entry: the scratch entry for a thread with no repo.
    pub repo: RepoId,
    /// Whether the user archived it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// When it was created, in RFC 3339 UTC.
    pub created_at: Timestamp,
    /// When a client last marked it seen with `thread/update` (0033). The app counts a run that
    /// stopped after this as one the user hasn't seen. Absent if never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub seen_at: Option<Timestamp>,
    /// Until when the user snoozed it (0033). A time in the past means it isn't snoozed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub snoozed_until: Option<Timestamp>,
    /// When its newest message was sent: its newest turn, or its creation (0033). Absent from a
    /// plxd without `threadAttention`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_prompt_at: Option<Timestamp>,
    /// The run that launched it (0041), from `thread/start`'s `parent`. Absent for a thread the
    /// user started, and once the parent is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent: Option<RunId>,
    /// The run and turn it was forked from (0041). Absent once that run is deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub forked_from: Option<ForkedFrom>,
    /// Its title (0041). Absent leaves it to the client, which shows its prompt's first line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    /// Whether the user or an agent marked it settled: nothing left to do (0041).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub settled: bool,
}

/// Where a thread was forked from: a run, and the turn of it the fork continues after (0041).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ForkedFrom {
    /// The run it was forked from.
    pub run: RunId,
    /// The turn of that run it was forked at.
    pub turn: TurnId,
}

/// Params of `thread/update`: marks a thread seen, or snoozes it (0033), behind the
/// `threadAttention` capability, or sets its title or settled flag (0041), behind
/// `threadLineage`.
///
/// `seen` sets `seenAt` to plxd's clock now. `snoozedUntil` replaces the snooze; a time in the
/// past ends it. A change appends `thread.updated`; an update that changes nothing appends none.
/// Fails with `threadNotFound` for an unknown thread.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUpdateParams {
    /// The thread's run id.
    pub run_id: RunId,
    /// True to mark it seen now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seen: bool,
    /// Snoozes it until this time, in RFC 3339 UTC.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub snoozed_until: Option<Timestamp>,
    /// Its new title, trimmed. Empty clears it. At most [`MAX_THREAD_TITLE_BYTES`] bytes, or it
    /// fails with `invalidParams`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    /// True to mark it settled, false to clear that.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub settled: Option<bool>,
}

/// Result of `thread/update`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUpdateResult {
    /// The thread as it stands.
    pub thread: Thread,
}

/// Params of `repo/update`: sets a repo entry's icon, which replaces the whole icon (0033),
/// behind the `threadAttention` capability. A change appends `repo.updated`. Fails with
/// `repoNotFound` for an unknown entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoUpdateParams {
    /// The entry's id.
    pub repo: RepoId,
    /// Its new icon, checked as `project/update` checks a project's.
    pub icon: ProjectIcon,
}

/// Result of `repo/update`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoUpdateResult {
    /// The entry as it stands.
    pub repo: Repo,
}

/// Params of `thread/list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListParams {}

/// Result of `thread/list`: every repo entry and every thread. The threads' runs come from
/// `agent/list` with each entry's id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadListResult {
    /// Every repo entry, oldest first.
    pub repos: Vec<Repo>,
    /// Every thread, oldest first.
    pub threads: Vec<Thread>,
    /// The `seq` of the last event the snapshot reflects. Subscribe to host-level events with
    /// `after` set to it.
    pub seq: u64,
}

/// Params of `repo/add`: registers a repository for normal threads.
///
/// Idempotent on `id`, and failing with `idConflict` if the same id comes with another path. A
/// path that is already registered returns its existing entry, whatever `id` says. The path must
/// be the top folder of a git working tree on the host, or it fails with `notARepository`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoAddParams {
    /// The new entry's id, a version 7 UUID generated by the client.
    pub id: RepoId,
    /// The absolute path of the repository on the host.
    pub path: String,
}

/// Result of `repo/add`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoAddResult {
    /// The entry, new or existing.
    pub repo: Repo,
}

/// Params of `thread/start`: starts a normal thread's agent in its own worktree, as `agent/start`
/// starts a worker. With `approvals`, a Claude thread is full Claude Code in every mode, with no
/// worker sandbox (0034); without it, it keeps the worker's sandbox (0013).
///
/// Idempotent on `runId`: the same id with the same params returns the run; with different
/// params it fails with `idConflict`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    /// The new run's id, a version 7 UUID generated by the client.
    pub run_id: RunId,
    /// The repo entry to work in. Absent starts a thread with no repo, in a scratch repository
    /// of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub repo: Option<RepoId>,
    /// The run that launches it, recorded as its parent (0041). It must exist, or the start fails
    /// with `runNotFound`. Behind `threadLineage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent: Option<RunId>,
    /// Its title, as `thread/update` takes it. Not part of what makes a retry with the same run id
    /// conflict, since the title can change. Behind `threadLineage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub title: Option<String>,
    /// The first message.
    pub prompt: String,
    /// The account to run on. Absent means the worker role's default (`accounts/defaults/*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<AccountChoice>,
    /// The model, as `agent/start` takes it. Absent means the CLI's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    /// How hard the model thinks, as `agent/start` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub effort: Option<AgentEffort>,
    /// Claude Code's permission mode for the agent, as `agent/start` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub permission: Option<AgentPermission>,
    /// The context window in tokens, as `agent/start` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_window: Option<u32>,
    /// Fast mode on or off, as `agent/start` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fast: Option<bool>,
    /// Names the worktree's branch `parallax/<branchSlug>`: lowercase letters, digits, and hyphens,
    /// no leading or trailing hyphen, at most 40 bytes. A branch that already has the name gets
    /// the run's short id after it. Absent names it `parallax/<short run id>`. Not part of what makes
    /// a retry with the same run id conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub branch_slug: Option<String>,
    /// Images for the first message, as `agent/start`'s.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<PromptImage>,
    /// Forward the agent's permission requests to the client, as `agent/start` takes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approvals: bool,
    /// Work in the repo entry's own checkout, on whatever branch it has out, instead of in a new
    /// worktree: the agent's changes land there, uncommitted, and `branchSlug` is ignored. Needs a
    /// `repo`. Behind the `checkout` capability, since an older plxd would silently ignore it and
    /// make a worktree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checkout: bool,
    /// The ref the new worktree starts from, such as `develop` or `origin/develop`. Absent means
    /// the repo's `HEAD`. Not with `checkout`. Behind the `repoRefs` capability, like
    /// `checkoutRef`. Not part of what makes a retry with the same run id conflict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub base: Option<String>,
    /// With `checkout`, the branch to switch the checkout to before the agent starts, as
    /// `git switch` does: a remote-tracking ref such as `origin/foo` switches to the local `foo`,
    /// made to track it if missing. Never forced: when git refuses, such as over local changes
    /// it would overwrite, the start fails with git's reason. Absent switches nothing. Not part of
    /// what makes a retry with the same run id conflict, and a retry switches nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub checkout_ref: Option<String>,
    /// Threads attached to the first message as context, as `agent/start` takes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<RunId>,
}

/// Params of `thread/search`: finds threads by what was said in them (PLX-372, decision 0047),
/// behind the `threadContext` capability.
///
/// Matches `query` anywhere in a thread's messages: the user's, Parallax's wake-ups, and the
/// agent's replies, but not its tool calls. Case-insensitive for ASCII letters. An empty query
/// fails with `invalidParams`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSearchParams {
    /// The text to find.
    pub query: String,
    /// The most threads to return: 20 by default, and at most 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub limit: Option<u32>,
}

/// Result of `thread/search`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSearchResult {
    /// The matching threads, the one with the newest message first.
    pub threads: Vec<Thread>,
}

/// Params of `thread/fork`: a new thread that continues thread `runId`'s conversation from one of
/// its turns, behind the `threadFork` capability (0050). The fork has no CLI until its first
/// `agent/send`. Its transcript starts with the parent's up to the end of that turn.
///
/// It works where the parent does: a new worktree cut from the parent's latest commit, the same
/// checkout for a Current checkout thread, or a scratch repository of its own, holding the
/// parent's latest commit, for a thread with no repo.
///
/// Idempotent on `newRunId`: a retry returns the fork, and a run id that is taken by anything
/// else fails with `idConflict`. Fails with `threadNotFound` for an unknown parent, and with
/// `invalidParams` for a turn the parent doesn't have or one it is still running.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadForkParams {
    /// The thread to fork.
    pub run_id: RunId,
    /// The fork's run id, a version 7 UUID generated by the client.
    pub new_run_id: RunId,
    /// The turn to fork at: a follow-up's turn id, or the parent's run id for its prompt's turn.
    /// Absent means its latest turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<TurnId>,
    /// The account to run on. Absent means the one the parent's session is on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<AccountChoice>,
    /// The model. Absent means the parent's, when the fork runs on the parent's backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
}

/// Params of `repo/refs`, behind the `repoRefs` capability. Fails with `repoNotFound` for an
/// unknown entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoRefsParams {
    /// The entry's id.
    pub repo: RepoId,
}

/// Result of `repo/refs`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct RepoRefsResult {
    /// Its local and remote-tracking branches, `origin/HEAD` left out: the default branch first,
    /// then the most recently committed first.
    pub refs: Vec<RepoRef>,
}

/// A branch in a repository, as `repo/refs` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts about a branch, not states of one thing"
)]
pub struct RepoRef {
    /// Its short name: `develop`, or `origin/develop` for a remote-tracking branch.
    pub name: String,
    /// True for a remote-tracking branch.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remote: bool,
    /// True for the default branch `origin/HEAD` names: its local branch, or the remote-tracking
    /// one when there is no local one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
    /// True for the branch the repository's checkout has out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub current: bool,
    /// True for a branch checked out in another worktree, a thread's included, which the checkout
    /// can't switch to.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worktree: bool,
}

/// Result of `thread/start`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResult {
    /// The thread.
    pub thread: Thread,
    /// Its run, as it stands.
    pub run: AgentRun,
}

/// Params of `thread/archive`: archives a thread or brings it back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadArchiveParams {
    /// The thread's run id.
    pub run_id: RunId,
    /// True to archive it, false to bring it back.
    pub archived: bool,
}

/// Result of `thread/archive`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadArchiveResult {
    /// The thread as it stands.
    pub thread: Thread,
}

/// Params of `thread/delete`: deletes a thread, its run, its worktree and branch, a thread with
/// no repo's scratch repository, and its stored events.
///
/// A running CLI is cancelled first, and the delete answers once it has exited and its changes
/// were committed. Deleting a thread that doesn't exist fails with `threadNotFound`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDeleteParams {
    /// The thread's run id.
    pub run_id: RunId,
}

/// Result of `thread/delete`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDeleteResult {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Repo, RepoId, Thread};
    use crate::RunId;

    #[test]
    fn false_flags_are_left_out() {
        let repo = Repo {
            id: RepoId::generate(),
            name: "parallax".to_owned(),
            path: "/Users/me/src/parallax".to_owned(),
            scratch: false,
            icon: None,
            created_at: "2026-09-26T12:00:00Z".parse().unwrap(),
        };
        let value = serde_json::to_value(&repo).unwrap();
        assert!(value.get("scratch").is_none(), "{value}");
        let thread = Thread {
            id: RunId::generate(),
            repo: repo.id,
            archived: false,
            created_at: repo.created_at,
            seen_at: None,
            snoozed_until: None,
            last_prompt_at: None,
            parent: None,
            forked_from: None,
            title: None,
            settled: false,
        };
        let value = serde_json::to_value(&thread).unwrap();
        assert!(value.get("archived").is_none(), "{value}");
        assert!(value.get("settled").is_none(), "{value}");
        let archived: Thread = serde_json::from_value(json!({
            "id": thread.id,
            "repo": repo.id,
            "archived": true,
            "createdAt": "2026-09-26T12:00:00Z",
        }))
        .unwrap();
        assert!(archived.archived);
    }
}
