//! `thread/list`, `repo/add`, `thread/start`, `thread/archive`, and `thread/delete` (#110),
//! behind the `threads` capability. The logic is [`crate::threads`].

use std::sync::Arc;

use serde_json::Value;
use wisp_protocol::jsonrpc::{ErrorObject, Request};
use wisp_protocol::methods::{
    RepoAdd, RequestMethod, ThreadArchive, ThreadDelete, ThreadList, ThreadStart,
};
use wisp_protocol::{
    RepoAddParams, RepoAddResult, ThreadArchiveParams, ThreadArchiveResult, ThreadDeleteParams,
    ThreadDeleteResult, ThreadListParams, ThreadListResult, ThreadStartParams, ThreadStartResult,
};

use super::{Context, handle};
use crate::threads;

/// Whether `method` is one of this module's: a `thread/*` or `repo/*` method.
pub(crate) fn handles(method: &str) -> bool {
    method.starts_with("thread/") || method.starts_with("repo/")
}

/// Answers a `thread/*` or `repo/*` method.
pub(crate) async fn dispatch(context: &Context, request: &Request) -> Result<Value, ErrorObject> {
    match request.method.as_str() {
        ThreadList::NAME => handle::<ThreadList, _, _>(request, |p| list(context, p)).await,
        RepoAdd::NAME => handle::<RepoAdd, _, _>(request, |p| add_repo(context, p)).await,
        ThreadStart::NAME => handle::<ThreadStart, _, _>(request, |p| start(context, p)).await,
        ThreadArchive::NAME => {
            handle::<ThreadArchive, _, _>(request, |p| archive(context, p)).await
        }
        ThreadDelete::NAME => handle::<ThreadDelete, _, _>(request, |p| delete(context, p)).await,
        other => Err(ErrorObject::method_not_found(other)),
    }
}

async fn list(context: &Context, _: ThreadListParams) -> Result<ThreadListResult, ErrorObject> {
    threads::list(&context.daemon).await
}

async fn add_repo(context: &Context, params: RepoAddParams) -> Result<RepoAddResult, ErrorObject> {
    threads::add_repo(&context.daemon, params).await
}

// Starting and deleting run detached from the request, as `agent/*` does, so a dropped
// connection never leaves a thread half created or half deleted.

async fn start(
    context: &Context,
    params: ThreadStartParams,
) -> Result<ThreadStartResult, ErrorObject> {
    super::agent::check_text("prompt", &params.prompt)?;
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(threads::start(daemon, params))
        .await
}

async fn archive(
    context: &Context,
    params: ThreadArchiveParams,
) -> Result<ThreadArchiveResult, ErrorObject> {
    threads::archive(&context.daemon, params).await
}

async fn delete(
    context: &Context,
    params: ThreadDeleteParams,
) -> Result<ThreadDeleteResult, ErrorObject> {
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(async move { threads::delete(&daemon, params.run_id).await })
        .await
}
