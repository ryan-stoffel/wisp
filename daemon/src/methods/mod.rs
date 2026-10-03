//! The methods plxd answers, one module per group, routed through `parallax_protocol`'s method
//! table so every params and result type is the protocol's own.
//!
//! Each capability gets a module here (M3 `agents`: `agent.rs` and `context.rs`; M4
//! `coordinator`: `project/start` in `project.rs`; #110 `threads`: `thread.rs`; RYA-227
//! `projectEdit`: `project/update` in `project.rs`; PLX-338 `projectDelete`: `project/delete` in
//! `project.rs`; PLX-318 `pullRequests` and PLX-328 `prDiff`: `pr.rs`; PLX-359 `composerMenus`:
//! `composer.rs`; PLX-336 `githubStatus`: `github/status` in `accounts.rs`; PLX-401 `inbox`:
//! `inbox.rs`), and `host.rs` advertises the capability in `initialize`.

mod accounts;
mod agent;
mod composer;
mod context;
mod defaults;
mod events;
mod host;
pub(crate) mod inbox;
mod pr;
pub(crate) mod project;
mod thread;
mod usage;

use std::future::{Future, ready};
use std::sync::Arc;

use parallax_protocol::jsonrpc::{ErrorObject, INVALID_REQUEST, Request, RequestId, Response};
use parallax_protocol::methods::{
    AccountsDefaultsGet, AccountsDefaultsSet, AccountsKeysAdd, AccountsKeysList,
    AccountsKeysRemove, AccountsList, AccountsRefresh, AgentAccept, AgentApprove, AgentAutoResume,
    AgentCancel, AgentCommands, AgentCommit, AgentDiff, AgentEvents, AgentFile, AgentFiles,
    AgentGitStatus, AgentImage, AgentList, AgentOpenPr, AgentPush, AgentRequestChanges,
    AgentResumeNow, AgentSend, AgentStart, ContextList, ContextRead, ContextWrite, EventsSubscribe,
    EventsUnsubscribe, GithubStatusGet, HostHealth, HostSettingsGet, HostSettingsSet, HostVersion,
    InboxList, InboxSeen, Initialize, PrAct, PrDiff, PrView, ProjectCreate, ProjectDelete,
    ProjectList, ProjectStart, ProjectUpdate, RequestMethod, UsageDaily, UsageGet, UsageHistory,
};
use parallax_protocol::{EventsSubscribeResult, EventsUnsubscribeResult, SubscriptionId};
use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub(crate) use defaults::read_defaults;
pub(crate) use events::{Cursor, Cursors};
pub(crate) use host::{Session, initialize, os_version};

use crate::server::Daemon;

/// What a request handler has to work with.
pub(crate) struct Context {
    pub daemon: Arc<Daemon>,
    /// Cancelled by `$/cancelRequest`, or when the connection closes.
    pub cancel: CancellationToken,
}

/// What the connection's writer sends for a request.
#[derive(Debug)]
pub(crate) enum Reply {
    Response(Response),
    /// Send the response, then start delivering the subscription's events.
    Subscribe {
        response: Response,
        cursor: Cursor,
    },
    /// Stop delivering the subscription's events, then send the response.
    Unsubscribe {
        response: Response,
        subscription: SubscriptionId,
    },
}

/// Answers a request on an initialized connection.
pub(crate) async fn dispatch(context: Context, request: Request) -> Reply {
    let id = request.id.clone();
    if context.cancel.is_cancelled() {
        return Reply::Response(Response::error(Some(id), ErrorObject::request_cancelled()));
    }
    let result = match request.method.as_str() {
        HostHealth::NAME => {
            handle::<HostHealth, _, _>(&request, |p| ready(Ok(host::health(&context, p)))).await
        }
        HostVersion::NAME => {
            handle::<HostVersion, _, _>(&request, |p| ready(Ok(host::version(&context, p)))).await
        }
        name if name.starts_with("host/settings/") => host_settings_method(&context, &request)
            .await
            .unwrap_or_else(|| Err(ErrorObject::method_not_found(name))),
        name if project_scoped(name) => project_method(&context, &request)
            .await
            .unwrap_or_else(|| Err(ErrorObject::method_not_found(name))),
        AccountsList::NAME => {
            handle::<AccountsList, _, _>(&request, |p| accounts::list(&context, p)).await
        }
        AccountsRefresh::NAME => {
            handle::<AccountsRefresh, _, _>(&request, |p| accounts::refresh(&context, p)).await
        }
        AccountsKeysAdd::NAME => {
            handle::<AccountsKeysAdd, _, _>(&request, |p| accounts::keys::add(&context, p)).await
        }
        AccountsKeysList::NAME => {
            handle::<AccountsKeysList, _, _>(&request, |p| accounts::keys::list(&context, p)).await
        }
        AccountsKeysRemove::NAME => {
            handle::<AccountsKeysRemove, _, _>(&request, |p| accounts::keys::remove(&context, p))
                .await
        }
        name if name.starts_with("usage/") => usage_method(&context, &request)
            .await
            .unwrap_or_else(|| Err(ErrorObject::method_not_found(name))),
        AccountsDefaultsGet::NAME => {
            handle::<AccountsDefaultsGet, _, _>(&request, |p| defaults::get(&context, p)).await
        }
        AccountsDefaultsSet::NAME => {
            handle::<AccountsDefaultsSet, _, _>(&request, |p| defaults::set(&context, p)).await
        }
        ContextList::NAME => {
            handle::<ContextList, _, _>(&request, |p| context::list(&context, p)).await
        }
        ContextRead::NAME => {
            handle::<ContextRead, _, _>(&request, |p| context::read(&context, p)).await
        }
        ContextWrite::NAME => {
            handle::<ContextWrite, _, _>(&request, |p| context::write(&context, p)).await
        }
        name if name.starts_with("agent/") => agent_method(&context, &request)
            .await
            .unwrap_or_else(|| Err(ErrorObject::method_not_found(name))),
        PrView::NAME => handle::<PrView, _, _>(&request, |p| pr::view(&context, p)).await,
        PrAct::NAME => handle::<PrAct, _, _>(&request, |p| pr::act(&context, p)).await,
        PrDiff::NAME => handle::<PrDiff, _, _>(&request, |p| pr::diff(&context, p)).await,
        GithubStatusGet::NAME => {
            handle::<GithubStatusGet, _, _>(&request, |p| accounts::github(&context, p)).await
        }
        name if thread::handles(name) => thread::dispatch(&context, &request).await,
        EventsSubscribe::NAME => {
            let subscribed = match request.params() {
                Ok(params) => events::subscribe(&context, params).await,
                Err(error) => Err(error),
            };
            return match subscribed {
                Ok(cursor) => Reply::Subscribe {
                    response: success(
                        id,
                        &EventsSubscribeResult {
                            subscription: cursor.subscription,
                        },
                    ),
                    cursor,
                },
                Err(error) => Reply::Response(Response::error(Some(id), error)),
            };
        }
        EventsUnsubscribe::NAME => {
            return match request.params::<<EventsUnsubscribe as RequestMethod>::Params>() {
                Ok(params) => Reply::Unsubscribe {
                    response: success(id, &EventsUnsubscribeResult {}),
                    subscription: params.subscription,
                },
                Err(error) => Reply::Response(Response::error(Some(id), error)),
            };
        }
        Initialize::NAME => Err(ErrorObject::new(
            INVALID_REQUEST,
            "Invalid request: the connection is already initialized",
        )),
        other => Err(ErrorObject::method_not_found(other)),
    };
    Reply::Response(Response {
        id: Some(id),
        result,
    })
}

/// Answers a `host/settings/*` method (PLX-371), or `None` if there is no such method.
async fn host_settings_method(
    context: &Context,
    request: &Request,
) -> Option<Result<Value, ErrorObject>> {
    Some(match request.method.as_str() {
        HostSettingsGet::NAME => {
            handle::<HostSettingsGet, _, _>(request, |p| host::settings(context, p)).await
        }
        HostSettingsSet::NAME => {
            handle::<HostSettingsSet, _, _>(request, |p| host::set_settings(context, p)).await
        }
        _ => return None,
    })
}

/// Answers a `usage/*` method, or `None` if there is no such method.
async fn usage_method(context: &Context, request: &Request) -> Option<Result<Value, ErrorObject>> {
    Some(match request.method.as_str() {
        UsageGet::NAME => handle::<UsageGet, _, _>(request, |p| usage::get(context, p)).await,
        UsageHistory::NAME => {
            handle::<UsageHistory, _, _>(request, |p| usage::history(context, p)).await
        }
        UsageDaily::NAME => handle::<UsageDaily, _, _>(request, |p| usage::daily(context, p)).await,
        _ => return None,
    })
}

/// Whether `name` is a `project/*` method or a Project's `inbox/*` one (PLX-401).
fn project_scoped(name: &str) -> bool {
    name.starts_with("project/") || name.starts_with("inbox/")
}

/// Answers a `project/*` method (RYA-227) or a Project's `inbox/*` one (PLX-401), or `None` if
/// there is no such method.
async fn project_method(
    context: &Context,
    request: &Request,
) -> Option<Result<Value, ErrorObject>> {
    Some(match request.method.as_str() {
        ProjectList::NAME => {
            handle::<ProjectList, _, _>(request, |p| project::list(context, p)).await
        }
        ProjectCreate::NAME => {
            handle::<ProjectCreate, _, _>(request, |p| project::create(context, p)).await
        }
        ProjectStart::NAME => {
            handle::<ProjectStart, _, _>(request, |p| project::start(context, p)).await
        }
        ProjectUpdate::NAME => {
            handle::<ProjectUpdate, _, _>(request, |p| project::update(context, p)).await
        }
        ProjectDelete::NAME => {
            handle::<ProjectDelete, _, _>(request, |p| project::delete(context, p)).await
        }
        InboxList::NAME => handle::<InboxList, _, _>(request, |p| inbox::list(context, p)).await,
        InboxSeen::NAME => handle::<InboxSeen, _, _>(request, |p| inbox::seen(context, p)).await,
        _ => return None,
    })
}

/// Answers an `agent/*` method (#156, #157, RYA-191, RYA-222), or `None` if there is no such
/// method.
async fn agent_method(context: &Context, request: &Request) -> Option<Result<Value, ErrorObject>> {
    Some(match request.method.as_str() {
        AgentStart::NAME => handle::<AgentStart, _, _>(request, |p| agent::start(context, p)).await,
        AgentSend::NAME => handle::<AgentSend, _, _>(request, |p| agent::send(context, p)).await,
        AgentCancel::NAME => {
            handle::<AgentCancel, _, _>(request, |p| agent::cancel(context, p)).await
        }
        AgentList::NAME => handle::<AgentList, _, _>(request, |p| agent::list(context, p)).await,
        AgentEvents::NAME => {
            handle::<AgentEvents, _, _>(request, |p| agent::events(context, p)).await
        }
        AgentImage::NAME => handle::<AgentImage, _, _>(request, |p| agent::image(context, p)).await,
        AgentDiff::NAME => handle::<AgentDiff, _, _>(request, |p| agent::diff(context, p)).await,
        AgentFile::NAME => handle::<AgentFile, _, _>(request, |p| agent::file(context, p)).await,
        AgentFiles::NAME => handle::<AgentFiles, _, _>(request, |p| agent::files(context, p)).await,
        AgentAccept::NAME => {
            handle::<AgentAccept, _, _>(request, |p| agent::accept(context, p)).await
        }
        AgentRequestChanges::NAME => {
            handle::<AgentRequestChanges, _, _>(request, |p| agent::request_changes(context, p))
                .await
        }
        AgentOpenPr::NAME => {
            handle::<AgentOpenPr, _, _>(request, |p| agent::open_pr(context, p)).await
        }
        AgentApprove::NAME => {
            handle::<AgentApprove, _, _>(request, |p| agent::approve(context, p)).await
        }
        AgentGitStatus::NAME => {
            handle::<AgentGitStatus, _, _>(request, |p| agent::git_status(context, p)).await
        }
        AgentCommit::NAME => {
            handle::<AgentCommit, _, _>(request, |p| agent::commit(context, p)).await
        }
        AgentPush::NAME => handle::<AgentPush, _, _>(request, |p| agent::push(context, p)).await,
        AgentCommands::NAME => {
            handle::<AgentCommands, _, _>(request, |p| composer::list_commands(context, p)).await
        }
        AgentResumeNow::NAME => {
            handle::<AgentResumeNow, _, _>(request, |p| agent::resume_now(context, p)).await
        }
        AgentAutoResume::NAME => {
            handle::<AgentAutoResume, _, _>(request, |p| agent::auto_resume(context, p)).await
        }
        _ => return None,
    })
}

async fn handle<M, F, Fut>(request: &Request, handler: F) -> Result<Value, ErrorObject>
where
    M: RequestMethod,
    F: FnOnce(M::Params) -> Fut,
    Fut: Future<Output = Result<M::Result, ErrorObject>>,
{
    let params = request.params::<M::Params>()?;
    let result = handler(params).await?;
    serde_json::to_value(result).map_err(ErrorObject::internal_error)
}

/// A successful response with `result`.
pub(crate) fn success(id: RequestId, result: &impl Serialize) -> Response {
    match serde_json::to_value(result) {
        Ok(value) => Response::success(id, value),
        Err(error) => Response::error(Some(id), ErrorObject::internal_error(error)),
    }
}
