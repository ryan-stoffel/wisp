//! `thread/list`, `repo/add`, `thread/start`, `thread/archive`, and `thread/delete` (#110),
//! behind the `threads` capability, `thread/fork` (0050), behind `threadFork`, `thread/update`
//! and `repo/update` (0033), behind `threadAttention` (and `threadLineage` for its title and
//! settled flag, 0041), `repo/refs`, behind `repoRefs`, and `thread/search` (PLX-372), behind
//! `threadContext`. The logic is [`crate::threads`].
//! `repo/files` is `composer.rs`'s, behind `composerMenus`.

use std::sync::Arc;

use parallax_protocol::jsonrpc::{ErrorObject, Request};
use parallax_protocol::methods::{
    RepoAdd, RepoFiles, RepoRefs, RepoUpdate, RequestMethod, ThreadArchive, ThreadDelete,
    ThreadFork, ThreadList, ThreadSearch, ThreadStart, ThreadUpdate,
};
use parallax_protocol::{
    RepoAddParams, RepoAddResult, RepoRefsParams, RepoRefsResult, RepoUpdateParams,
    RepoUpdateResult, ThreadArchiveParams, ThreadArchiveResult, ThreadDeleteParams,
    ThreadDeleteResult, ThreadForkParams, ThreadListParams, ThreadListResult, ThreadSearchParams,
    ThreadSearchResult, ThreadStartParams, ThreadStartResult, ThreadUpdateParams,
    ThreadUpdateResult,
};
use serde_json::Value;

use super::{Context, handle};
use crate::{agents, threads};

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
        ThreadFork::NAME => handle::<ThreadFork, _, _>(request, |p| fork(context, p)).await,
        ThreadArchive::NAME => {
            handle::<ThreadArchive, _, _>(request, |p| archive(context, p)).await
        }
        ThreadUpdate::NAME => handle::<ThreadUpdate, _, _>(request, |p| update(context, p)).await,
        RepoUpdate::NAME => handle::<RepoUpdate, _, _>(request, |p| update_repo(context, p)).await,
        ThreadDelete::NAME => handle::<ThreadDelete, _, _>(request, |p| delete(context, p)).await,
        RepoRefs::NAME => handle::<RepoRefs, _, _>(request, |p| refs(context, p)).await,
        ThreadSearch::NAME => handle::<ThreadSearch, _, _>(request, |p| search(context, p)).await,
        RepoFiles::NAME => {
            handle::<RepoFiles, _, _>(request, |p| super::composer::files(context, p)).await
        }
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
    mut params: ThreadStartParams,
) -> Result<ThreadStartResult, ErrorObject> {
    super::agent::check_message("prompt", &params.prompt, &params.images)?;
    params.threads = agents::attached::check(&context.daemon, params.threads).await?;
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(threads::start(daemon, params))
        .await
}

async fn fork(
    context: &Context,
    params: ThreadForkParams,
) -> Result<ThreadStartResult, ErrorObject> {
    let daemon = Arc::clone(&context.daemon);
    context
        .daemon
        .agents
        .detached(threads::fork(daemon, params))
        .await
}

async fn archive(
    context: &Context,
    params: ThreadArchiveParams,
) -> Result<ThreadArchiveResult, ErrorObject> {
    threads::archive(&context.daemon, params).await
}

async fn update(
    context: &Context,
    params: ThreadUpdateParams,
) -> Result<ThreadUpdateResult, ErrorObject> {
    threads::update(&context.daemon, params).await
}

async fn search(
    context: &Context,
    params: ThreadSearchParams,
) -> Result<ThreadSearchResult, ErrorObject> {
    threads::search(&context.daemon, params).await
}

async fn refs(context: &Context, params: RepoRefsParams) -> Result<RepoRefsResult, ErrorObject> {
    threads::refs(&context.daemon, params).await
}

async fn update_repo(
    context: &Context,
    params: RepoUpdateParams,
) -> Result<RepoUpdateResult, ErrorObject> {
    threads::update_repo(&context.daemon, params).await
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
