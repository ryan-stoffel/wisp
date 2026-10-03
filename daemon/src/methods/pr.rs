//! `pr/view` and `pr/act` (PLX-318), behind the `pullRequests` capability, `pr/diff`
//! (PLX-328), behind `prDiff`, and `pr/link` and `pr/unlink` (0041), behind `threadTools`. The runner reads and acts on a run's linked pull requests in
//! [`crate::agents`].

use std::sync::Arc;

use parallax_protocol::jsonrpc::ErrorObject;
use parallax_protocol::{
    AgentRunResult, PrActParams, PrAction, PrDiffResult, PrViewParams, PullRequest,
};

use super::Context;
use crate::agents;

/// `pr/view`.
pub(crate) async fn view(
    context: &Context,
    params: PrViewParams,
) -> Result<PullRequest, ErrorObject> {
    agents::view_pr(Arc::clone(&context.daemon), params).await
}

/// `pr/diff`.
pub(crate) async fn diff(
    context: &Context,
    params: PrViewParams,
) -> Result<PrDiffResult, ErrorObject> {
    agents::diff_pr(Arc::clone(&context.daemon), params).await
}

/// `pr/link` with `linked`, or `pr/unlink` without.
pub(crate) async fn link(
    context: &Context,
    params: PrViewParams,
    linked: bool,
) -> Result<AgentRunResult, ErrorObject> {
    let PrViewParams { run_id, url } = params;
    let daemon = Arc::clone(&context.daemon);
    let run = agents::link_pr(daemon, run_id, url, linked).await?;
    Ok(AgentRunResult { run })
}

/// `pr/act`: an action this version knows, checked here. Detached, so a merge `gh` has started
/// still ends and is logged when the client gives up waiting.
pub(crate) async fn act(
    context: &Context,
    params: PrActParams,
) -> Result<PullRequest, ErrorObject> {
    if params.action == PrAction::Unknown {
        return Err(ErrorObject::invalid_params("unknown pull request action"));
    }
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(agents::act_pr(daemon, params))
        .await
}
