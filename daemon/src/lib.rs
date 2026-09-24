//! The wisp host daemon, `wispd`.
//!
//! `wispd serve` listens on a per-user Unix socket and speaks the protocol from the
//! `wisp-protocol` crate (decision record 0007). The editor reaches it through `wispd attach`,
//! locally or over SSH.
//!
//! The library holds what the subcommands share:
//!
//! - [`paths`]: the data folder, the files in it, and the socket path rule, which `attach` must
//!   follow too.
//! - [`logging`]: the log file and its level.
//! - [`server`]: the server behind `wispd serve`.

#![warn(missing_docs)]

mod event_log;
pub mod logging;
mod methods;
pub mod paths;
pub mod server;
mod store;

/// wispd's release version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
