//! `thread/list`, `repo/add`, `thread/start`, `thread/archive`, and `thread/delete` (#110),
//! behind the `threads` capability. The logic is [`crate::threads`].

use serde_json::Value;
use wisp_protocol::jsonrpc::{ErrorObject, Request};
use wisp_protocol::methods::{
    RepoAdd, RequestMethod, ThreadArchive, ThreadDelete, ThreadList, ThreadStart,
};
use wisp_protocol::{ThreadDeleteParams, ThreadDeleteResult, ThreadStartParams, ThreadStartResult};

use super::{Context, handle};
use crate::threads;

/// Answers a `thread/*` or `repo/*` method.
pub(crate) async fn dispatch(context: &Context, request: &Request) -> Result<Value, ErrorObject> {
    let daemon = &context.daemon;
    match request.method.as_str() {
        ThreadList::NAME => handle::<ThreadList, _, _>(request, |_| threads::list(daemon)).await,
        RepoAdd::NAME => handle::<RepoAdd, _, _>(request, |p| threads::add_repo(daemon, p)).await,
        ThreadStart::NAME => handle::<ThreadStart, _, _>(request, |p| start(context, p)).await,
        ThreadArchive::NAME => {
            handle::<ThreadArchive, _, _>(request, |p| threads::archive(daemon, p)).await
        }
        ThreadDelete::NAME => handle::<ThreadDelete, _, _>(request, |p| delete(context, p)).await,
        other => Err(ErrorObject::method_not_found(other)),
    }
}

// Starting and deleting run detached from the request, as `agent/*` does, so a dropped
// connection never leaves a thread half created or half deleted.

async fn start(
    context: &Context,
    params: ThreadStartParams,
) -> Result<ThreadStartResult, ErrorObject> {
    super::agent::check_text("prompt", &params.prompt)?;
    context
        .detached(|daemon| threads::start(daemon, params))
        .await
}

async fn delete(
    context: &Context,
    params: ThreadDeleteParams,
) -> Result<ThreadDeleteResult, ErrorObject> {
    context
        .detached(|daemon| async move { threads::delete(&daemon, params.run_id).await })
        .await
}
