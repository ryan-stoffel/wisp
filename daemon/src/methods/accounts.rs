//! `accounts/list` and `accounts/refresh` (#114): the vendor CLIs wispd detects, gated on the
//! `agentClis` capability.
//!
//! `keys` holds `accounts/keys/add`, `accounts/keys/list`, and `accounts/keys/remove` (#117),
//! which manage stored API keys under the separate `accounts` capability. The two features share
//! the `accounts/` method prefix but are otherwise independent, so they live in one module here
//! without colliding: this file's `list` detects CLIs, `keys::list` reads key accounts.

pub(crate) mod keys;

use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AccountsListParams, AccountsListResult, AccountsRefreshParams, AccountsRefreshResult,
};

use super::Context;

/// The detected CLIs, from a short-lived cache when one is fresh.
pub(crate) async fn list(
    context: &Context,
    _: AccountsListParams,
) -> Result<AccountsListResult, ErrorObject> {
    let probe = context.daemon.cli_detector.list().await;
    Ok(AccountsListResult {
        clis: probe.clis,
        checked_at: probe.checked_at,
    })
}

/// The detected CLIs, always freshly probed.
pub(crate) async fn refresh(
    context: &Context,
    _: AccountsRefreshParams,
) -> Result<AccountsRefreshResult, ErrorObject> {
    let probe = context.daemon.cli_detector.refresh().await;
    Ok(AccountsRefreshResult {
        clis: probe.clis,
        checked_at: probe.checked_at,
    })
}
