//! The method table: every method, with its params and result types.
//!
//! Each method is a marker type that implements [`RequestMethod`] or [`NotificationMethod`].
//! The same table produces the generated TypeScript's method maps, so a method cannot exist on
//! one side only. Later milestones add methods here, each gated on a capability.
//!
//! ```
//! use parallax_protocol::HostHealthParams;
//! use parallax_protocol::jsonrpc::{ErrorObject, Request};
//! use parallax_protocol::methods::{HostHealth, Initialize, RequestMethod};
//!
//! let request = Request::new::<HostHealth>(1, HostHealthParams {});
//! assert_eq!(request.method, HostHealth::NAME);
//!
//! let answer = match request.method.as_str() {
//!     Initialize::NAME => Ok("handshake"),
//!     HostHealth::NAME => Ok("health"),
//!     other => Err(ErrorObject::method_not_found(other)),
//! };
//! assert_eq!(answer, Ok("health"));
//! ```

use serde::Serialize;
use serde::de::DeserializeOwned;
use ts_rs::TS;

use crate::jsonrpc::CancelRequestParams;
use crate::{
    AccountsDefaultsGetParams, AccountsDefaultsGetResult, AccountsDefaultsSetParams,
    AccountsKeysAddParams, AccountsKeysAddResult, AccountsKeysListParams, AccountsKeysListResult,
    AccountsKeysRemoveParams, AccountsKeysRemoveResult, AccountsListParams, AccountsListResult,
    AccountsRefreshParams, AccountsRefreshResult, AgentAcceptParams, AgentAcceptResult,
    AgentApproveParams, AgentApproveResult, AgentAutoResumeParams, AgentCancelParams,
    AgentCommandsParams, AgentCommandsResult, AgentCommitParams, AgentDiffParams, AgentDiffResult,
    AgentEventsParams, AgentEventsResult, AgentFileParams, AgentFileResult, AgentFilesParams,
    AgentFilesResult, AgentGitStatusParams, AgentImageParams, AgentListParams, AgentListResult,
    AgentOpenPrParams, AgentOpenPrResult, AgentPushParams, AgentRequestChangesParams,
    AgentResumeNowParams, AgentRunResult, AgentSendParams, AgentStartParams, ContextListParams,
    ContextListResult, ContextReadParams, ContextReadResult, ContextWriteParams,
    ContextWriteResult, EventsEventParams, EventsSubscribeParams, EventsSubscribeResult,
    EventsUnsubscribeParams, EventsUnsubscribeResult, GitStatus, GithubStatus, GithubStatusParams,
    HostHealthParams, HostHealthResult, HostSettings, HostSettingsGetParams, HostSettingsSetParams,
    HostVersionParams, HostVersionResult, InitializeParams, InitializeResult, PrActParams,
    PrDiffResult, PrViewParams, ProjectCreateParams, ProjectCreateResult, ProjectDeleteParams,
    ProjectDeleteResult, ProjectListParams, ProjectListResult, ProjectStartParams,
    ProjectUpdateParams, ProjectUpdateResult, PromptImage, PullRequest, RepoAddParams,
    RepoAddResult, RepoFilesParams, RepoFilesResult, RepoRefsParams, RepoRefsResult,
    RepoUpdateParams, RepoUpdateResult, ThreadArchiveParams, ThreadArchiveResult,
    ThreadDeleteParams, ThreadDeleteResult, ThreadListParams, ThreadListResult, ThreadSearchParams,
    ThreadSearchResult, ThreadStartParams, ThreadStartResult, ThreadUpdateParams,
    ThreadUpdateResult, UsageDailyParams, UsageDailyResult, UsageGetParams, UsageGetResult,
    UsageHistoryParams, UsageHistoryResult,
};

/// A method that is called with a request and answered with a response.
pub trait RequestMethod {
    /// The method name on the wire.
    const NAME: &'static str;
    /// The request's params.
    type Params: Serialize + DeserializeOwned + TS + 'static;
    /// The result of a successful response.
    type Result: Serialize + DeserializeOwned + TS + 'static;
}

/// A method that is sent as a notification, with no response.
pub trait NotificationMethod {
    /// The method name on the wire.
    const NAME: &'static str;
    /// The notification's params.
    type Params: Serialize + DeserializeOwned + TS + 'static;
}

pub(crate) trait Visitor {
    fn request<M: RequestMethod>(&mut self, docs: &[&str]);
    fn notification<N: NotificationMethod>(&mut self, docs: &[&str]);
}

macro_rules! method_table {
    (
        requests {
            $(
                $(#[doc = $request_doc:literal])*
                $request:ident = $request_name:literal: $params:ty => $result:ty;
            )*
        }
        notifications {
            $(
                $(#[doc = $notification_doc:literal])*
                $notification:ident = $notification_name:literal: $notification_params:ty;
            )*
        }
    ) => {
        $(
            $(#[doc = $request_doc])*
            #[derive(Debug)]
            pub enum $request {}

            impl RequestMethod for $request {
                const NAME: &'static str = $request_name;
                type Params = $params;
                type Result = $result;
            }
        )*

        $(
            $(#[doc = $notification_doc])*
            #[derive(Debug)]
            pub enum $notification {}

            impl NotificationMethod for $notification {
                const NAME: &'static str = $notification_name;
                type Params = $notification_params;
            }
        )*

        pub(crate) fn visit(visitor: &mut impl Visitor) {
            $(visitor.request::<$request>(&[$($request_doc),*]);)*
            $(visitor.notification::<$notification>(&[$($notification_doc),*]);)*
        }
    };
}

method_table! {
    requests {
        /// `initialize`: the handshake. It must be the first request on a connection; plxd
        /// answers anything before it with `notInitialized`.
        Initialize = "initialize": InitializeParams => InitializeResult;
        /// `host/health`: uptime, store state, and running agents. The app sends it every
        /// 30 seconds and on wake, as a heartbeat.
        HostHealth = "host/health": HostHealthParams => HostHealthResult;
        /// `host/version`: plxd's release and protocol versions, operating system, and CPU
        /// architecture.
        HostVersion = "host/version": HostVersionParams => HostVersionResult;
        /// `project/list`: every project, and the `seq` the list reflects.
        ProjectList = "project/list": ProjectListParams => ProjectListResult;
        /// `project/create`: creates a project, idempotent on its client-generated id.
        ProjectCreate = "project/create": ProjectCreateParams => ProjectCreateResult;
        /// `events/subscribe`: replays the events after a `seq`, then streams new ones as
        /// `events/event` notifications.
        EventsSubscribe = "events/subscribe": EventsSubscribeParams => EventsSubscribeResult;
        /// `events/unsubscribe`: ends a subscription.
        EventsUnsubscribe = "events/unsubscribe": EventsUnsubscribeParams => EventsUnsubscribeResult;
        /// `accounts/keys/add`: stores an API key in the Keychain, idempotent on its
        /// client-generated id. The response returns only the key's masked form.
        AccountsKeysAdd = "accounts/keys/add": AccountsKeysAddParams => AccountsKeysAddResult;
        /// `accounts/keys/list`: every key account, with its key masked.
        AccountsKeysList = "accounts/keys/list": AccountsKeysListParams => AccountsKeysListResult;
        /// `accounts/keys/remove`: removes a key account's key from the Keychain, and its
        /// record. Fails with `accountNotFound` if the id does not exist.
        AccountsKeysRemove = "accounts/keys/remove": AccountsKeysRemoveParams => AccountsKeysRemoveResult;
        /// `accounts/list`: the vendor CLIs plxd detects (#114), installed, signed in, and their
        /// plan where exposed. May answer from a short-lived cache. Gated on the `agentClis`
        /// capability.
        AccountsList = "accounts/list": AccountsListParams => AccountsListResult;
        /// `accounts/refresh`: like `accounts/list`, but always probes again instead of using the
        /// cache. Gated on the `agentClis` capability.
        AccountsRefresh = "accounts/refresh": AccountsRefreshParams => AccountsRefreshResult;
        /// `usage/get`: per-account tokens and cost for today and this week (local time on this
        /// host), and the latest limit windows.
        UsageGet = "usage/get": UsageGetParams => UsageGetResult;
        /// `usage/history`: tokens and cost since a time, summed per UTC hour, account, and
        /// model, and each account's run count over the same range.
        UsageHistory = "usage/history": UsageHistoryParams => UsageHistoryResult;
        /// `usage/daily`: every Claude Code, Codex, and Cursor session's tokens and cost on this
        /// host since a local day, per local day, agent, and model (0039), and each source that
        /// failed.
        UsageDaily = "usage/daily": UsageDailyParams => UsageDailyResult;
        /// `accounts/defaults/get`: this host's default account for the coordinator role and for
        /// a worker role, absent where none is set (#119).
        AccountsDefaultsGet = "accounts/defaults/get": AccountsDefaultsGetParams => AccountsDefaultsGetResult;
        /// `accounts/defaults/set`: sets or clears one role's default account, and returns both
        /// roles' defaults as they stand after the change.
        AccountsDefaultsSet = "accounts/defaults/set": AccountsDefaultsSetParams => AccountsDefaultsGetResult;
        /// `context/list`: a project's shared context files (0005), each with its size, when it
        /// was last modified, and who last wrote it, if known.
        ContextList = "context/list": ContextListParams => ContextListResult;
        /// `context/read`: one shared context file's content. Fails with `contextNotFound` if it
        /// does not exist.
        ContextRead = "context/read": ContextReadParams => ContextReadResult;
        /// `context/write`: writes a shared context file in full, idempotent on its
        /// client-generated id. The last write to a path wins when two race.
        ContextWrite = "context/write": ContextWriteParams => ContextWriteResult;
        /// `agent/start`: starts a worker in its own worktree of the project's repository, with
        /// the worker sandbox (0013) and the shared context folder, idempotent on its
        /// client-generated run id. Gated on the `agents` capability, like every `agent/*`
        /// method.
        AgentStart = "agent/start": AgentStartParams => AgentRunResult;
        /// `agent/send`: a message to a run (0011): its next turn while it runs, or a resumed
        /// session once it has ended. Idempotent on the message's client-generated turn id.
        AgentSend = "agent/send": AgentSendParams => AgentRunResult;
        /// `agent/cancel`: stops a running agent. Does nothing to a run that isn't running.
        AgentCancel = "agent/cancel": AgentCancelParams => AgentRunResult;
        /// `agent/list`: every run, or one project's, and the `seq` the list reflects.
        AgentList = "agent/list": AgentListParams => AgentListResult;
        /// `agent/events`: one run's events from plxd's log, a page at a time.
        AgentEvents = "agent/events": AgentEventsParams => AgentEventsResult;
        /// `agent/image`: an image sent with one of a run's messages, by an id from its
        /// `turnStarted`. Gated on the `promptImages` capability.
        AgentImage = "agent/image": AgentImageParams => PromptImage;
        /// `agent/diff`: the files that differ between a run's base and its latest commit, each
        /// with its stats and a size-capped unified diff (#157). Gated on the `agentReview`
        /// capability, like every review method.
        AgentDiff = "agent/diff": AgentDiffParams => AgentDiffResult;
        /// `agent/file`: one file of a run's diff, on its base or head side, base64-encoded and
        /// size-capped, for a diff editor. Its `working` side, behind the `files` capability,
        /// reads the file on disk now.
        AgentFile = "agent/file": AgentFileParams => AgentFileResult;
        /// `agent/files`: one folder of a run's worktree, or a Current checkout thread's
        /// checkout, without `.git` or what git ignores, for browsing (RYA-296). Gated on the
        /// `files` capability.
        AgentFiles = "agent/files": AgentFilesParams => AgentFilesResult;
        /// `agent/accept`: merges a run's commit into the project repository's current branch on
        /// the host, fast-forward when possible, then removes its worktree and branch. Never
        /// pushes. Idempotent on its client-generated id.
        AgentAccept = "agent/accept": AgentAcceptParams => AgentAcceptResult;
        /// `agent/requestChanges`: the reviewer's follow-up to a run, sent as `agent/send` sends
        /// a message. Idempotent on its client-generated turn id.
        AgentRequestChanges = "agent/requestChanges": AgentRequestChangesParams => AgentRunResult;
        /// `agent/openPr`: pushes a finished run's branch to the repository's `origin` and opens
        /// a pull request for it with `gh`, or finds the one already open (RYA-168). Gated on the
        /// `openPr` capability.
        AgentOpenPr = "agent/openPr": AgentOpenPrParams => AgentOpenPrResult;
        /// `agent/gitStatus`: the git state of a run's folder (RYA-298). Gated on the `git`
        /// capability, like `agent/commit` and `agent/push`.
        AgentGitStatus = "agent/gitStatus": AgentGitStatusParams => GitStatus;
        /// `agent/commit`: stages everything in a finished run's folder and commits it.
        AgentCommit = "agent/commit": AgentCommitParams => GitStatus;
        /// `agent/push`: pushes a finished run's branch to `origin`, setting its upstream.
        AgentPush = "agent/push": AgentPushParams => GitStatus;
        /// `agent/approve`: answers a run's permission request, from its `approvalRequested`
        /// item, by allowing or denying the tool call (RYA-222, decision 0031). Idempotent on the
        /// request. Gated on the `approvals` capability.
        AgentApprove = "agent/approve": AgentApproveParams => AgentApproveResult;
        /// `thread/list`: every repo entry and normal thread, and the `seq` the list reflects
        /// (#110). Gated on the `threads` capability, like every `thread/*` and `repo/*` method.
        ThreadList = "thread/list": ThreadListParams => ThreadListResult;
        /// `repo/add`: registers a repository on the host for normal threads, idempotent on its
        /// client-generated id and on its path.
        RepoAdd = "repo/add": RepoAddParams => RepoAddResult;
        /// `thread/start`: starts a normal thread's agent in a worktree of a repo entry, or in a
        /// scratch repository of its own when no repo is given. Idempotent on its
        /// client-generated run id.
        ThreadStart = "thread/start": ThreadStartParams => ThreadStartResult;
        /// `thread/archive`: archives a normal thread or brings it back.
        ThreadArchive = "thread/archive": ThreadArchiveParams => ThreadArchiveResult;
        /// `thread/update`: marks a normal thread seen or snoozes it (0033), gated on the
        /// `threadAttention` capability, or sets its title or settled flag (0041), gated on
        /// `threadLineage`.
        ThreadUpdate = "thread/update": ThreadUpdateParams => ThreadUpdateResult;
        /// `repo/update`: sets a repo entry's icon (0033). Gated on the `threadAttention`
        /// capability.
        RepoUpdate = "repo/update": RepoUpdateParams => RepoUpdateResult;
        /// `thread/delete`: deletes a normal thread with its run, worktree, and stored events,
        /// stopping its CLI first if it runs.
        ThreadDelete = "thread/delete": ThreadDeleteParams => ThreadDeleteResult;
        /// `project/start`: starts a project's coordinator chat, a no-write run in its repository
        /// with plxd's coordinator tools (0024), idempotent on its client-generated run id. It
        /// replaces the project's last coordinator unless that one is running. Gated on the
        /// `coordinator` capability.
        ProjectStart = "project/start": ProjectStartParams => AgentRunResult;
        /// `project/update`: renames a project or sets its icon, and leaves its `updatedAt` as
        /// it is (0032). Fails with `projectNotFound` for an unknown project. Gated on the
        /// `projectEdit` capability, like `Project.icon`.
        ProjectUpdate = "project/update": ProjectUpdateParams => ProjectUpdateResult;
        /// `repo/refs`: a repo entry's local and remote-tracking branches, for picking the ref a
        /// thread starts from. Gated on the `repoRefs` capability.
        RepoRefs = "repo/refs": RepoRefsParams => RepoRefsResult;
        /// `pr/view`: one of a run's linked pull requests as GitHub has it now, read with `gh`
        /// (PLX-318). Gated on the `pullRequests` capability, like `pr/act`.
        PrView = "pr/view": PrViewParams => PullRequest;
        /// `pr/act`: merges, squashes, sets auto-merge on or off, drafts, readies, or closes one
        /// of a run's linked pull requests with `gh`, and returns it as it is after.
        PrAct = "pr/act": PrActParams => PullRequest;
        /// `pr/diff`: one of a run's linked pull requests' unified diff, read with `gh pr diff`
        /// and cut at a size cap (PLX-328). Gated on the `prDiff` capability.
        PrDiff = "pr/diff": PrViewParams => PrDiffResult;
        /// `pr/link`: links a GitHub pull request URL to a run, unless it already is, and reports
        /// the run as `agent.updated` (0041). Fails with `invalidParams` for another URL. Gated
        /// on the `threadTools` capability, like `pr/unlink`.
        PrLink = "pr/link": PrViewParams => AgentRunResult;
        /// `pr/unlink`: removes a pull request URL from a run's links. Unlinking one that isn't
        /// linked changes nothing.
        PrUnlink = "pr/unlink": PrViewParams => AgentRunResult;
        /// `project/delete`: deletes a project with every run in it, stopping their CLIs first
        /// (PLX-338). Fails with `projectNotFound` for an unknown project or a repo entry's id.
        /// Gated on the `projectDelete` capability.
        ProjectDelete = "project/delete": ProjectDeleteParams => ProjectDeleteResult;
        /// `agent/commands`: a CLI's own slash commands and skills, for the composer's `/` menu
        /// (PLX-359). Gated on the `composerMenus` capability, like `repo/files`.
        AgentCommands = "agent/commands": AgentCommandsParams => AgentCommandsResult;
        /// `repo/files`: a thread's files that git tracks or doesn't ignore, capped, for the
        /// composer's `@` menu.
        RepoFiles = "repo/files": RepoFilesParams => RepoFilesResult;
        /// `github/status`: the GitHub CLI (`gh`) on the host, whether it is signed in to
        /// github.com, and as whom (PLX-336). Read-only and never prompts. Gated on the
        /// `githubStatus` capability.
        GithubStatusGet = "github/status": GithubStatusParams => GithubStatus;
        /// `thread/search`: the host's threads whose messages contain a query, the one with the
        /// newest message first (PLX-372). Gated on the `threadContext` capability.
        ThreadSearch = "thread/search": ThreadSearchParams => ThreadSearchResult;
        /// `agent/resumeNow`: resumes a run waiting for its usage limit to reset now (PLX-371,
        /// decision 0049). Gated on the `autoResume` capability, like `agent/autoResume` and
        /// `host/settings/*`.
        AgentResumeNow = "agent/resumeNow": AgentResumeNowParams => AgentRunResult;
        /// `agent/autoResume`: sets or clears a run's auto-resume override.
        AgentAutoResume = "agent/autoResume": AgentAutoResumeParams => AgentRunResult;
        /// `host/settings/get`: this host's settings.
        HostSettingsGet = "host/settings/get": HostSettingsGetParams => HostSettings;
        /// `host/settings/set`: changes this host's settings and returns them.
        HostSettingsSet = "host/settings/set": HostSettingsSetParams => HostSettings;
    }
    notifications {
        /// `$/cancelRequest`: cancels a request, which still gets exactly one response. Either
        /// side may send it.
        CancelRequest = "$/cancelRequest": CancelRequestParams;
        /// `events/event`: one event for a subscription. plxd sends it.
        EventsEvent = "events/event": EventsEventParams;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{NotificationMethod, RequestMethod, Visitor, visit};

    #[derive(Default)]
    struct Names(Vec<&'static str>);

    impl Visitor for Names {
        fn request<M: RequestMethod>(&mut self, docs: &[&str]) {
            assert!(!docs.is_empty(), "{} has no docs", M::NAME);
            self.0.push(M::NAME);
        }

        fn notification<N: NotificationMethod>(&mut self, docs: &[&str]) {
            assert!(!docs.is_empty(), "{} has no docs", N::NAME);
            self.0.push(N::NAME);
        }
    }

    #[test]
    fn the_table_has_every_method_once() {
        let mut names = Names::default();
        visit(&mut names);
        assert_eq!(
            names.0,
            [
                "initialize",
                "host/health",
                "host/version",
                "project/list",
                "project/create",
                "events/subscribe",
                "events/unsubscribe",
                "accounts/keys/add",
                "accounts/keys/list",
                "accounts/keys/remove",
                "accounts/list",
                "accounts/refresh",
                "usage/get",
                "usage/history",
                "usage/daily",
                "accounts/defaults/get",
                "accounts/defaults/set",
                "context/list",
                "context/read",
                "context/write",
                "agent/start",
                "agent/send",
                "agent/cancel",
                "agent/list",
                "agent/events",
                "agent/image",
                "agent/diff",
                "agent/file",
                "agent/files",
                "agent/accept",
                "agent/requestChanges",
                "agent/openPr",
                "agent/gitStatus",
                "agent/commit",
                "agent/push",
                "agent/approve",
                "thread/list",
                "repo/add",
                "thread/start",
                "thread/archive",
                "thread/update",
                "repo/update",
                "thread/delete",
                "project/start",
                "project/update",
                "repo/refs",
                "pr/view",
                "pr/act",
                "pr/diff",
                "pr/link",
                "pr/unlink",
                "project/delete",
                "agent/commands",
                "repo/files",
                "github/status",
                "thread/search",
                "agent/resumeNow",
                "agent/autoResume",
                "host/settings/get",
                "host/settings/set",
                "$/cancelRequest",
                "events/event",
            ]
        );
        assert_eq!(names.0.iter().collect::<BTreeSet<_>>().len(), names.0.len());
    }
}
