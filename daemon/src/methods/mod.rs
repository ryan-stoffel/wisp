//! The methods wispd answers, one module per group, routed through `wisp_protocol`'s method
//! table so every params and result type is the protocol's own.
//!
//! Later milestones add a module here for each capability (M3 `agents`: agents and context; M4
//! `coordinator`), and `host.rs` advertises the capability in `initialize`.

mod accounts;
mod events;
mod host;
mod project;
mod usage;

use std::future::{Future, ready};
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use wisp_protocol::jsonrpc::{ErrorObject, INVALID_REQUEST, Request, RequestId, Response};
use wisp_protocol::methods::{
    AccountsKeysAdd, AccountsKeysList, AccountsKeysRemove, AccountsList, AccountsRefresh,
    EventsSubscribe, EventsUnsubscribe, HostHealth, HostVersion, Initialize, ProjectCreate,
    ProjectList, RequestMethod, UsageGet,
};
use wisp_protocol::{EventsSubscribeResult, EventsUnsubscribeResult, SubscriptionId};

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
        ProjectList::NAME => {
            handle::<ProjectList, _, _>(&request, |p| project::list(&context, p)).await
        }
        ProjectCreate::NAME => {
            handle::<ProjectCreate, _, _>(&request, |p| project::create(&context, p)).await
        }
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
        UsageGet::NAME => handle::<UsageGet, _, _>(&request, |p| usage::get(&context, p)).await,
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
