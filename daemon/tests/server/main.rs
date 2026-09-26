//! Integration tests: real servers in temporary folders, driven through `wisp_protocol`.
//!
//! Most tests run the `wispd serve` binary. Tests of timers and limits that the command line
//! doesn't expose run the same server in-process with a shorter `Config`.

mod context;
mod events;
mod handshake;
mod lifecycle;
mod projects;
mod requests;
mod support;
mod usage;
