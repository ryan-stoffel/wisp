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
    AgentSendParams, AgentStartParams, AgentStatus, AgentTodoItem, AgentTodoStatus,
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
pub use usage::{AccountUsage, UsageGetParams, UsageGetResult, UsageLimitWindow, UsagePeriod};

/// The newest protocol version this crate speaks. Versions start at 1.
///
/// Additive changes keep it. Removals, renames, and type changes bump it, and the previous
/// version stays supported for at least one release.
pub const PROTOCOL_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fmt::Debug;

    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use serde_json::{Map, json};

    use super::*;
    use crate::jsonrpc::{CancelRequestParams, RequestId};

    fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(value: &T) {
        let json = serde_json::to_string(value).unwrap();
        assert_eq!(&serde_json::from_str::<T>(&json).unwrap(), value, "{json}");
    }

    fn project() -> Project {
        Project {
            id: ProjectId::generate(),
            name: "wisp".to_owned(),
            repo_path: "/Users/me/src/wisp".to_owned(),
            branch: Some("main".to_owned()),
            created_at: "2026-09-24T12:00:00Z".parse().unwrap(),
            updated_at: "2026-09-24T12:05:00.125Z".parse().unwrap(),
        }
    }

    fn key_account() -> KeyAccount {
        KeyAccount {
            id: AccountId::generate(),
            provider: Provider::Anthropic,
            label: "Personal".to_owned(),
            created_at: "2026-09-24T12:00:00Z".parse().unwrap(),
            masked_key: "sk-ant-...abcd".to_owned(),
        }
    }

    #[test]
    fn every_message_type_round_trips() {
        let mut options = Map::new();
        options.insert("level".to_owned(), json!(2));
        let capabilities = Capabilities(BTreeMap::from([
            ("agents".to_owned(), Map::new()),
            ("future".to_owned(), options),
        ]));
        for machine_id in [None, Some("m1".to_owned())] {
            round_trip(&InitializeParams {
                protocol: ProtocolRange { min: 1, max: 2 },
                client: ClientInfo {
                    name: "wisp".to_owned(),
                    version: "0.1.0".to_owned(),
                    machine_id,
                },
                capabilities: capabilities.clone(),
            });
        }
        round_trip(&InitializeResult {
            protocol: 1,
            wispd: "0.1.0".to_owned(),
            log_id: LogId::generate(),
            capabilities: Capabilities::default(),
            max_frame_bytes: 8_388_608,
        });
        round_trip(&HostHealthParams {});
        for store in [StoreState::Ok, StoreState::Unavailable] {
            round_trip(&HostHealthResult {
                uptime_seconds: 1,
                store,
                running_agents: 0,
            });
        }
        round_trip(&HostVersionParams {});
        round_trip(&HostVersionResult {
            wispd: "0.1.0".to_owned(),
            protocol: ProtocolRange::SUPPORTED,
            os: "macOS 27.0".to_owned(),
            arch: "aarch64".to_owned(),
        });
        round_trip(&ProjectListParams {});
        round_trip(&ProjectListResult {
            projects: vec![project(), project()],
            seq: (1 << 53) - 1,
        });
        round_trip(&ProjectCreateParams {
            id: ProjectId::generate(),
            name: "wisp".to_owned(),
            repo_path: "/".to_owned(),
        });
        round_trip(&ProjectCreateResult { project: project() });
        for project in [None, Some(ProjectId::generate())] {
            round_trip(&EventsSubscribeParams { after: 7, project });
            round_trip(&EventsEventParams {
                subscription: SubscriptionId::generate(),
                seq: 8,
                time: "2026-09-24T12:00:00Z".parse().unwrap(),
                project,
                event: WispEvent::ProjectCreated {
                    project: self::project(),
                },
            });
        }
        round_trip(&EventsSubscribeResult {
            subscription: SubscriptionId::generate(),
        });
        round_trip(&EventsUnsubscribeParams {
            subscription: SubscriptionId::generate(),
        });
        round_trip(&EventsUnsubscribeResult {});
        for id in [RequestId::Number(-3), RequestId::from("a")] {
            round_trip(&CancelRequestParams { id });
        }
        round_trip(&ErrorData {
            kind: ErrorKind::IdConflict,
            detail: Some(json!({"why": "name differs"})),
        });
        round_trip(&IncompatibleProtocolDetail {
            requested: ProtocolRange { min: 2, max: 2 },
            supported: ProtocolRange::SUPPORTED,
            wispd: "0.1.0".to_owned(),
        });
        round_trip(&AccountsKeysAddParams {
            id: AccountId::generate(),
            provider: Provider::Anthropic,
            label: "Personal".to_owned(),
            key: serde_json::from_value(json!("sk-ant-secret")).unwrap(),
        });
        round_trip(&AccountsKeysAddResult {
            account: key_account(),
        });
        round_trip(&AccountsKeysListParams {});
        round_trip(&AccountsKeysListResult {
            accounts: vec![key_account(), key_account()],
        });
        round_trip(&AccountsKeysRemoveParams {
            id: AccountId::generate(),
        });
        round_trip(&AccountsKeysRemoveResult {});
    }

    #[test]
    fn account_default_types_round_trip() {
        round_trip(&AccountsDefaultsGetParams {});
        for account in [
            None,
            Some(AccountChoice::Subscription {
                backend: "claude".to_owned(),
            }),
            Some(AccountChoice::Key {
                id: AccountId::generate(),
            }),
        ] {
            round_trip(&AccountsDefaultsSetParams {
                role: Role::Coordinator,
                account: account.clone(),
            });
        }
        round_trip(&AccountsDefaultsGetResult {
            coordinator: Some(AccountChoice::Subscription {
                backend: "claude".to_owned(),
            }),
            worker: Some(AccountChoice::Key {
                id: AccountId::generate(),
            }),
        });
        round_trip(&AccountsDefaultsGetResult::default());
    }

    #[test]
    fn context_types_round_trip() {
        for last_writer in [None, Some("editor".to_owned())] {
            round_trip(&context_file(last_writer));
        }
        round_trip(&ContextListParams {
            project: ProjectId::generate(),
        });
        round_trip(&ContextListResult {
            files: vec![context_file(None), context_file(Some("editor".to_owned()))],
        });
        round_trip(&ContextReadParams {
            project: ProjectId::generate(),
            path: "notes.md".to_owned(),
        });
        round_trip(&ContextReadResult {
            file: context_file(None),
            content: "# Notes".to_owned(),
        });
        for writer in [None, Some("editor".to_owned())] {
            round_trip(&ContextWriteParams {
                id: ContextWriteId::generate(),
                project: ProjectId::generate(),
                path: "notes.md".to_owned(),
                content: "# Notes".to_owned(),
                writer,
            });
        }
        round_trip(&ContextWriteResult {
            file: context_file(Some("editor".to_owned())),
        });
        round_trip(&EventsEventParams {
            subscription: SubscriptionId::generate(),
            seq: 9,
            time: "2026-09-24T12:00:00Z".parse().unwrap(),
            project: Some(ProjectId::generate()),
            event: WispEvent::ContextChanged {
                file: context_file(Some("editor".to_owned())),
            },
        });
    }

    fn context_file(last_writer: Option<String>) -> ContextFile {
        ContextFile {
            path: "notes.md".to_owned(),
            size: 7,
            modified_at: "2026-09-24T12:00:00Z".parse().unwrap(),
            last_writer,
        }
    }

    #[test]
    fn accounts_messages_round_trip() {
        round_trip(&AccountsListParams {});
        round_trip(&AccountsRefreshParams {});
        let clis = vec![
            DetectedCli {
                cli: CliKind::Claude,
                installed: true,
                path: Some("/usr/local/bin/claude".to_owned()),
                version: Some("2.1.281".to_owned()),
                signed_in: Some(true),
                auth_kind: Some(AuthKind::Subscription),
                plan: Some("max".to_owned()),
                note: None,
            },
            DetectedCli {
                cli: CliKind::Codex,
                installed: false,
                path: None,
                version: None,
                signed_in: None,
                auth_kind: None,
                plan: None,
                note: None,
            },
        ];
        round_trip(&AccountsListResult {
            clis: clis.clone(),
            checked_at: "2026-09-25T12:00:00Z".parse().unwrap(),
        });
        round_trip(&AccountsRefreshResult {
            clis,
            checked_at: "2026-09-25T12:00:00Z".parse().unwrap(),
        });
    }

    #[test]
    fn usage_types_round_trip_and_omit_what_is_not_reported() {
        round_trip(&UsageGetParams {});
        for cost in [None, Some(45_000)] {
            round_trip(&UsagePeriod {
                input_tokens: 100,
                output_tokens: 10,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd_micros: cost,
            });
        }
        for (used_percent, resets_at) in [(None, None), (Some(42.5), Some(project().created_at))] {
            round_trip(&UsageLimitWindow {
                window: "five_hour".to_owned(),
                used_percent,
                resets_at,
                captured_at: project().created_at,
            });
        }
        round_trip(&UsageGetResult {
            accounts: vec![AccountUsage {
                account_id: "claude-max".to_owned(),
                today: UsagePeriod {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    cost_usd_micros: None,
                },
                week: UsagePeriod {
                    input_tokens: 1,
                    output_tokens: 1,
                    cache_read_tokens: 0,
                    cache_write_tokens: 0,
                    cost_usd_micros: Some(1),
                },
                limits: Vec::new(),
            }],
        });
    }
}
