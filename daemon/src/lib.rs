//! The wisp host daemon, `wispd`.
//!
//! The library holds what the subcommands share:
//!
//! - [`paths`]: the data folder, the files in it, and the socket path rule, which `serve` and
//!   `attach` must both follow.
//! - [`logging`]: the log file and its level.

#![warn(missing_docs)]

pub mod logging;
pub mod paths;
