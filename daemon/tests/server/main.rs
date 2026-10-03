//! Integration tests: real servers in temporary folders, driven through `parallax_protocol`.
//!
//! Most tests run the `plxd serve` binary. Tests of timers and limits that the command line
//! doesn't expose run the same server in-process with a shorter `Config`.
//!
//! Unix only: they stop servers with signals, check sockets and modes, and run shell-script
//! CLIs. `tests/windows.rs` covers `serve` on Windows.
#![cfg(unix)]

mod agents;
mod approvals;
mod auto_resume;
mod context;
mod coordinator;
mod events;
mod git;
mod handshake;
mod inbox;
mod keys;
mod lifecycle;
mod mcp;
mod open_pr;
mod projects;
mod pull_requests;
mod requests;
mod support;
mod threads;
mod usage;
