//! The method table: every method, with its params and result types.
//!
//! Each method is a marker type that implements [`RequestMethod`] or [`NotificationMethod`].
//! The same table produces the generated TypeScript's method maps, so a method cannot exist on
//! one side only. Later milestones add methods here, each gated on a capability.
//!
//! ```
//! use wisp_protocol::HostHealthParams;
//! use wisp_protocol::jsonrpc::{ErrorObject, Request};
//! use wisp_protocol::methods::{HostHealth, Initialize, RequestMethod};
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
    AgentCancelParams, AgentDiffParams, AgentDiffResult, AgentEventsParams, AgentEventsResult,
    AgentFileParams, AgentFileResult, AgentListParams, AgentListResult, AgentRequestChangesParams,
    AgentRunResult, AgentSendParams, AgentStartParams, ContextListParams, ContextListResult,
    ContextReadParams, ContextReadResult, ContextWriteParams, ContextWriteResult,
    EventsEventParams, EventsSubscribeParams, EventsSubscribeResult, EventsUnsubscribeParams,
    EventsUnsubscribeResult, HostHealthParams, HostHealthResult, HostVersionParams,
    HostVersionResult, InitializeParams, InitializeResult, ProjectCreateParams,
    ProjectCreateResult, ProjectListParams, ProjectListResult, RepoAddParams, RepoAddResult,
    ThreadArchiveParams, ThreadArchiveResult, ThreadDeleteParams, ThreadDeleteResult,
    ThreadListParams, ThreadListResult, ThreadStartParams, ThreadStartResult, UsageGetParams,
    UsageGetResult,
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
        /// `initialize`: the handshake. It must be the first request on a connection; wispd
        /// answers anything before it with `notInitialized`.
        Initialize = "initialize": InitializeParams => InitializeResult;
        /// `host/health`: uptime, store state, and running agents. The editor sends it every
        /// 30 seconds and on wake, as a heartbeat.
        HostHealth = "host/health": HostHealthParams => HostHealthResult;
        /// `host/version`: wispd's release and protocol versions, operating system, and CPU
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
        /// `accounts/list`: the vendor CLIs wispd detects (#114), installed, signed in, and their
        /// plan where exposed. May answer from a short-lived cache. Gated on the `agentClis`
        /// capability.
        AccountsList = "accounts/list": AccountsListParams => AccountsListResult;
        /// `accounts/refresh`: like `accounts/list`, but always probes again instead of using the
        /// cache. Gated on the `agentClis` capability.
        AccountsRefresh = "accounts/refresh": AccountsRefreshParams => AccountsRefreshResult;
        /// `usage/get`: per-account tokens and cost for today and this week (local time on this
        /// host), and the latest limit windows.
        UsageGet = "usage/get": UsageGetParams => UsageGetResult;
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
        /// `agent/events`: one run's events from wispd's log, a page at a time.
        AgentEvents = "agent/events": AgentEventsParams => AgentEventsResult;
        /// `agent/diff`: the files that differ between a run's base and its latest commit, each
        /// with its stats and a size-capped unified diff (#157). Gated on the `agentReview`
        /// capability, like every review method.
        AgentDiff = "agent/diff": AgentDiffParams => AgentDiffResult;
        /// `agent/file`: one file of a run's diff, on its base or head side, base64-encoded and
        /// size-capped, for a diff editor.
        AgentFile = "agent/file": AgentFileParams => AgentFileResult;
        /// `agent/accept`: merges a run's commit into the project repository's current branch on
        /// the host, fast-forward when possible, then removes its worktree and branch. Never
        /// pushes. Idempotent on its client-generated id.
        AgentAccept = "agent/accept": AgentAcceptParams => AgentAcceptResult;
        /// `agent/requestChanges`: the reviewer's follow-up to a run, sent as `agent/send` sends
        /// a message. Idempotent on its client-generated turn id.
        AgentRequestChanges = "agent/requestChanges": AgentRequestChangesParams => AgentRunResult;
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
        /// `thread/delete`: deletes a normal thread with its run, worktree, and stored events.
        /// Refused with `runActive` while its CLI runs.
        ThreadDelete = "thread/delete": ThreadDeleteParams => ThreadDeleteResult;
    }
    notifications {
        /// `$/cancelRequest`: cancels a request, which still gets exactly one response. Either
        /// side may send it.
        CancelRequest = "$/cancelRequest": CancelRequestParams;
        /// `events/event`: one event for a subscription. wispd sends it.
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
                "agent/diff",
                "agent/file",
                "agent/accept",
                "agent/requestChanges",
                "thread/list",
                "repo/add",
                "thread/start",
                "thread/archive",
                "thread/delete",
                "$/cancelRequest",
                "events/event",
            ]
        );
        assert_eq!(names.0.iter().collect::<BTreeSet<_>>().len(), names.0.len());
    }
}
