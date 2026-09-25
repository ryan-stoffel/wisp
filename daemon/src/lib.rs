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
//! - [`launch_agent`]: the launch agent that `attach` starts wispd through, when it is installed.
//! - [`service`]: installs, removes, and reports on the per-user `LaunchAgent` that keeps
//!   `serve` running (#61).

#![warn(missing_docs)]

pub mod attach;
mod event_log;
pub mod launch_agent;
pub mod logging;
mod methods;
pub mod paths;
pub mod server;
pub mod service;
mod spawn;
mod store;

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
