//! The wisp host daemon, `wispd`.
//!
//! `wispd serve` listens on a per-user Unix socket and speaks the protocol from the
//! `wisp-protocol` crate (decision record 0007). The editor reaches it through `wispd attach`,
//! locally or over SSH.
//!
//! The library holds what the subcommands share:
//!
//! - [`paths`]: the data folder, the files in it, and the socket path rule, which `serve` and
//!   `attach` both follow.
//! - [`logging`]: the log file and its level.
//! - [`server`]: the server behind `wispd serve`.
//! - [`attach`]: reaching the server and bridging stdio to it, behind `wispd attach`.
//! - [`backend`]: the interface over the vendor CLIs that run agents (0004), and the process
//!   supervision they share.
//! - `context`: each project's shared context folder (0005, #155): `context/list`, `context/read`,
//!   `context/write`, and a watcher that turns an agent's own writes on disk into
//!   `context.changed` events.
//! - `detect`: detecting which vendor CLIs are installed and signed in, without touching their
//!   credentials (#114).
//! - [`launch_agent`]: the launch agent that `attach` starts wispd through, when it is installed.
//! - [`service`]: installs, removes, and reports on the per-user `LaunchAgent` that keeps
//!   `serve` running (#61).
//! - [`keystore`]: where API keys live, the macOS login Keychain (#117).
//! - [`usage`]: turns backend usage events into `wisp-store` rows (#120).
//! - `routing`: picks a task's backend and account, forces the coordinator's no-write policy,
//!   falls a failed subscription run back to a key account, and checks a coordinator's turn
//!   against the no-write policy (#119).
//! - [`worktree`]: creates, inspects, and removes the git worktrees agent runs use (#154).
//! - `agents`: the M3 runner behind `agent/*` (#156): starts a worker in its worktree, streams
//!   its events, commits its changes, and resumes it after a restart.
//! - `threads`: normal threads behind `thread/*` and `repo/*` (#110): runs with no coordinator
//!   that belong to a repo entry, or to a scratch repository for a thread with no repo.

#![warn(missing_docs)]

mod agents;
pub mod attach;
pub mod backend;
mod context;
mod detect;
mod event_log;
mod json;
pub mod keystore;
pub mod launch_agent;
pub mod logging;
mod methods;
pub mod paths;
mod repo;
pub mod routing;
pub mod server;
pub mod service;
mod spawn;
mod store;
mod threads;
pub mod usage;
pub mod worktree;

/// wispd's release version, reported by `wispd --version`, the protocol handshake
/// (`initialize` and `host/version`), and the `LaunchAgent`'s probe.
///
/// `scripts/editor/build-app` sets `WISP_VERSION` to the release version before it builds wispd
/// for the app bundle (0006, #44); everywhere else, including a plain `cargo build`, this falls
/// back to the crate's own placeholder in `Cargo.toml`.
pub const VERSION: &str = match option_env!("WISP_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
