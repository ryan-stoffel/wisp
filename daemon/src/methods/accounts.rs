//! `accounts/list` and `accounts/refresh` (#114): the vendor CLIs plxd detects, gated on the
//! `agentClis` capability. `github/status` (PLX-336), the GitHub CLI from the same detector, is
//! gated on `githubStatus`, and `github/install`, `github/signIn`, and `github/signInCancel`
//! (PLX-423), which set it up through [`crate::github`], on `githubSetup`.
//!
//! `keys` holds `accounts/keys/add`, `accounts/keys/list`, and `accounts/keys/remove` (#117),
//! which manage stored API keys under the separate `accounts` capability. The two features share
//! the `accounts/` method prefix but are otherwise independent, so they live in one module here
//! without colliding: this file's `list` detects CLIs, `keys::list` reads key accounts.

pub(crate) mod keys;

use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::{
    AccountsListParams, AccountsListResult, AccountsRefreshParams, AccountsRefreshResult,
    ErrorKind, GithubInstallParams, GithubSignIn, GithubSignInCancelParams,
    GithubSignInCancelResult, GithubSignInParams, GithubStatus, GithubStatusParams,
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

/// The GitHub CLI on the host, always freshly probed, with the install or sign-in in progress.
pub(crate) async fn github(
    context: &Context,
    _: GithubStatusParams,
) -> Result<GithubStatus, ErrorObject> {
    let mut status = context.daemon.cli_detector.github().await;
    context.daemon.github.fill(&mut status);
    Ok(status)
}

/// Starts installing `gh`, then answers with the status, which says it's installing.
pub(crate) async fn github_install(
    context: &Context,
    _: GithubInstallParams,
) -> Result<GithubStatus, ErrorObject> {
    context.daemon.github.install().map_err(setup_failed)?;
    github(context, GithubStatusParams {}).await
}

/// Starts signing `gh` in, or answers with the sign-in already pending.
pub(crate) async fn github_sign_in(
    context: &Context,
    _: GithubSignInParams,
) -> Result<GithubSignIn, ErrorObject> {
    context.daemon.github.sign_in().await.map_err(setup_failed)
}

/// Stops a pending sign-in.
pub(crate) fn github_sign_in_cancel(
    context: &Context,
    _: GithubSignInCancelParams,
) -> GithubSignInCancelResult {
    context.daemon.github.cancel_sign_in();
    GithubSignInCancelResult {}
}

fn setup_failed(message: String) -> ErrorObject {
    ErrorObject::parallax(ErrorKind::GithubSetupFailed, message)
}
