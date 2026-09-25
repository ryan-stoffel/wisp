//! `accounts/list` and `accounts/refresh` (#114): the vendor CLIs wispd detects, gated on the
//! `agentClis` capability. Distinct from #117's `accounts/keys/*`, which manages stored API keys.

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
