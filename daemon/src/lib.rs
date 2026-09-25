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
//! - [`launch_agent`]: the launch agent that `attach` starts wispd through, when it is installed.
//! - [`service`]: installs, removes, and reports on the per-user `LaunchAgent` that keeps
//!   `serve` running (#61).

#![warn(missing_docs)]

pub mod attach;
pub mod backend;
mod event_log;
pub mod launch_agent;
pub mod logging;
mod methods;
pub mod paths;
pub mod server;
pub mod service;
mod spawn;
mod store;

/// wispd's release version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
