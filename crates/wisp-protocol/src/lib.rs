//! The protocol between the editor and `wispd`, from decision record 0007.
//!
//! This crate is the single source of truth for it. The editor's TypeScript types are generated
//! from the types here (see [`typescript`]).
//!
//! - [`framing`]: newline-delimited JSON, one compact message per line, at most
//!   [`framing::MAX_FRAME_BYTES`] per line.
//! - [`jsonrpc`]: the JSON-RPC 2.0 envelope, and the error codes that malformed traffic gets.
//! - [`methods`]: the method table, which names every method with its params and result types.
//! - The crate root: the M1 messages, events, and wisp errors.
//!
//! # Changing the protocol
//!
//! Changes must be additive: new methods, notifications, event kinds, optional fields, and enum
//! values keep [`PROTOCOL_VERSION`]. Receivers ignore anything unknown, and every enum that Rust
//! receives has a fallback variant for values it does not know.
//!
//! The committed samples in `samples/v<N>/`, one directory per protocol version, enforce this in
//! `cargo test`. Each file is one scenario: a JSON array of the messages on the wire, in order.
//!
//! - Samples are append-only. Never edit or delete one; add a new file instead.
//! - Every message must decode into its typed form and encode back to exactly the same JSON. So
//!   renaming, removing, or retyping a field fails a test, and a field added later has to be
//!   optional and left out when absent, which is what an older peer sends anyway.
//! - Files in `lenient/` hold messages from a newer peer, with fields, methods, and values this
//!   version does not know. They only have to decode.
//! - Together, the exact samples must use every method, error code, error kind, event kind, and
//!   store state, so a new one needs a sample.

#![warn(missing_docs)]

mod account;
mod agent;
mod cli_account;
mod context;
mod defaults;
mod error;
mod events;
pub mod framing;
mod handshake;
mod host;
mod id;
pub mod jsonrpc;
pub mod methods;
mod project;
mod review;
mod thread;
pub mod typescript;
mod usage;

#[cfg(test)]
mod samples;

pub use account::{
    AccountId, AccountsKeysAddParams, AccountsKeysAddResult, AccountsKeysListParams,
    AccountsKeysListResult, AccountsKeysRemoveParams, AccountsKeysRemoveResult, KeyAccount,
    Provider, RawKey,
};
pub use agent::{
    AgentCancelParams, AgentEventsParams, AgentEventsResult, AgentFailureKind, AgentListParams,
    AgentListResult, AgentOutcome, AgentOutputItem, AgentPolicy, AgentRun, AgentRunResult,
    AgentRunState, AgentSendParams, AgentStartParams, AgentStatus, AgentTodoItem, AgentTodoStatus,
    AgentToolStatus, DiffSummary, LoggedEvent, RunId, TurnId,
};
pub use cli_account::{
    AccountsListParams, AccountsListResult, AccountsRefreshParams, AccountsRefreshResult, AuthKind,
    CliKind, DetectedCli,
};
pub use context::{
    ContextFile, ContextListParams, ContextListResult, ContextReadParams, ContextReadResult,
    ContextWriteId, ContextWriteParams, ContextWriteResult,
};
pub use defaults::{
    AccountChoice, AccountsDefaultsGetParams, AccountsDefaultsGetResult, AccountsDefaultsSetParams,
    Role,
};
pub use error::{ErrorData, ErrorKind, IncompatibleProtocolDetail};
pub use events::{
    EventsEventParams, EventsSubscribeParams, EventsSubscribeResult, EventsUnsubscribeParams,
    EventsUnsubscribeResult, LogId, SubscriptionId, WispEvent,
};
pub use handshake::{
    Capabilities, ClientInfo, InitializeParams, InitializeProtocol, InitializeResult, ProtocolRange,
};
pub use host::{
    HostHealthParams, HostHealthResult, HostVersionParams, HostVersionResult, StoreState,
};
pub use id::InvalidId;
pub use project::{
    Project, ProjectCreateParams, ProjectCreateResult, ProjectId, ProjectListParams,
    ProjectListResult,
};
pub use review::{
    AcceptId, AgentAcceptParams, AgentAcceptResult, AgentDiffFile, AgentDiffParams,
    AgentDiffResult, AgentDiffStats, AgentFileParams, AgentFileResult, AgentFileSide,
    AgentFileStatus, AgentMerge, AgentMergeKind, AgentRequestChangesParams,
};
pub use thread::{
    Repo, RepoAddParams, RepoAddResult, RepoId, Thread, ThreadArchiveParams, ThreadArchiveResult,
    ThreadDeleteParams, ThreadDeleteResult, ThreadListParams, ThreadListResult, ThreadStartParams,
    ThreadStartResult,
};
pub use usage::{AccountUsage, UsageGetParams, UsageGetResult, UsageLimitWindow, UsagePeriod};

/// The newest protocol version this crate speaks. Versions start at 1.
///
/// Additive changes keep it. Removals, renames, and type changes bump it, and the previous
/// version stays supported for at least one release.
pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    use super::*;

    fn decode<T: DeserializeOwned>(value: Value) -> T {
        serde_json::from_value(value).unwrap()
    }

    // The committed samples cover every known value; these are the values a newer peer adds.
    #[test]
    fn unknown_values_decode_as_unknown() {
        assert_eq!(decode::<Provider>(json!("gemini")), Provider::Unknown);
        assert_eq!(decode::<CliKind>(json!("gemini-cli")), CliKind::Unknown);
        assert_eq!(decode::<AuthKind>(json!("sso")), AuthKind::Unknown);
        assert_eq!(decode::<AgentStatus>(json!("paused")), AgentStatus::Unknown);
        assert_eq!(
            decode::<AgentOutputItem>(json!({"kind": "image", "url": "x"})),
            AgentOutputItem::Unknown
        );
        assert_eq!(
            decode::<AgentOutcome>(json!({"status": "merged"})),
            AgentOutcome::Unknown
        );
        assert_eq!(
            decode::<AgentFileSide>(json!("merged")),
            AgentFileSide::Unknown
        );
        assert_eq!(
            decode::<AgentFileStatus>(json!("unmerged")),
            AgentFileStatus::Unknown
        );
        assert_eq!(
            decode::<AgentMergeKind>(json!("rebase")),
            AgentMergeKind::Unknown
        );
        assert_eq!(
            decode::<AccountChoice>(json!({"kind": "quantum", "qubit": 1})),
            AccountChoice::Unknown
        );
        assert_eq!(
            decode::<WispEvent>(json!({"kind": "trigger.fired", "triggerId": "x"})),
            WispEvent::Unknown
        );
    }
}
