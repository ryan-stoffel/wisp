//! The method table: every method, with its params and result types.
//!
//! Each method is a marker type that implements [`RequestMethod`] or [`NotificationMethod`].
//! The same table produces the generated TypeScript's method maps, so a method cannot exist on
//! one side only. Later milestones add methods here, each gated on a capability.
//!
//! ```
//! use wisp_protocol::HostHealthParams;
//! use wisp_protocol::jsonrpc::{ErrorObject, Request};
//! use wisp_protocol::methods::{HostHealth, Initialize, RequestMethod};
//!
//! let request = Request::new::<HostHealth>(1, HostHealthParams {});
//! assert_eq!(request.method, HostHealth::NAME);
//!
//! let answer = match request.method.as_str() {
//!     Initialize::NAME => Ok("handshake"),
//!     HostHealth::NAME => Ok("health"),
//!     other => Err(ErrorObject::method_not_found(other)),
//! };
//! assert_eq!(answer, Ok("health"));
//! ```

use serde::Serialize;
use serde::de::DeserializeOwned;
use ts_rs::TS;

use crate::jsonrpc::CancelRequestParams;
use crate::{
    EventsEventParams, EventsSubscribeParams, EventsSubscribeResult, EventsUnsubscribeParams,
    EventsUnsubscribeResult, HostHealthParams, HostHealthResult, HostVersionParams,
    HostVersionResult, InitializeParams, InitializeResult, ProjectCreateParams,
    ProjectCreateResult, ProjectListParams, ProjectListResult,
};

/// A method that is called with a request and answered with a response.
pub trait RequestMethod {
    /// The method name on the wire.
    const NAME: &'static str;
    /// The request's params.
    type Params: Serialize + DeserializeOwned + TS + 'static;
    /// The result of a successful response.
    type Result: Serialize + DeserializeOwned + TS + 'static;
}

/// A method that is sent as a notification, with no response.
pub trait NotificationMethod {
    /// The method name on the wire.
    const NAME: &'static str;
    /// The notification's params.
    type Params: Serialize + DeserializeOwned + TS + 'static;
}

pub(crate) trait Visitor {
    fn request<M: RequestMethod>(&mut self, docs: &[&str]);
    fn notification<N: NotificationMethod>(&mut self, docs: &[&str]);
}

macro_rules! method_table {
    (
        requests {
            $(
                $(#[doc = $request_doc:literal])*
                $request:ident = $request_name:literal: $params:ty => $result:ty;
            )*
        }
        notifications {
            $(
                $(#[doc = $notification_doc:literal])*
                $notification:ident = $notification_name:literal: $notification_params:ty;
            )*
        }
    ) => {
        $(
            $(#[doc = $request_doc])*
            #[derive(Debug)]
            pub enum $request {}

            impl RequestMethod for $request {
                const NAME: &'static str = $request_name;
                type Params = $params;
                type Result = $result;
            }
        )*

        $(
            $(#[doc = $notification_doc])*
            #[derive(Debug)]
            pub enum $notification {}

            impl NotificationMethod for $notification {
                const NAME: &'static str = $notification_name;
                type Params = $notification_params;
            }
        )*

        pub(crate) fn visit(visitor: &mut impl Visitor) {
            $(visitor.request::<$request>(&[$($request_doc),*]);)*
            $(visitor.notification::<$notification>(&[$($notification_doc),*]);)*
        }
    };
}

method_table! {
    requests {
        /// `initialize`: the handshake. It must be the first request on a connection; wispd
        /// answers anything before it with `notInitialized`.
        Initialize = "initialize": InitializeParams => InitializeResult;
        /// `host/health`: uptime, store state, and running agents. The editor sends it every
        /// 30 seconds and on wake, as a heartbeat.
        HostHealth = "host/health": HostHealthParams => HostHealthResult;
        /// `host/version`: wispd's release and protocol versions, operating system, and CPU
        /// architecture.
        HostVersion = "host/version": HostVersionParams => HostVersionResult;
        /// `project/list`: every project, and the `seq` the list reflects.
        ProjectList = "project/list": ProjectListParams => ProjectListResult;
        /// `project/create`: creates a project, idempotent on its client-generated id.
        ProjectCreate = "project/create": ProjectCreateParams => ProjectCreateResult;
        /// `events/subscribe`: replays the events after a `seq`, then streams new ones as
        /// `events/event` notifications.
        EventsSubscribe = "events/subscribe": EventsSubscribeParams => EventsSubscribeResult;
        /// `events/unsubscribe`: ends a subscription.
        EventsUnsubscribe = "events/unsubscribe": EventsUnsubscribeParams => EventsUnsubscribeResult;
    }
    notifications {
        /// `$/cancelRequest`: cancels a request, which still gets exactly one response. Either
        /// side may send it.
        CancelRequest = "$/cancelRequest": CancelRequestParams;
        /// `events/event`: one event for a subscription. wispd sends it.
        EventsEvent = "events/event": EventsEventParams;
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{NotificationMethod, RequestMethod, Visitor, visit};

    #[derive(Default)]
    struct Names(Vec<&'static str>);

    impl Visitor for Names {
        fn request<M: RequestMethod>(&mut self, docs: &[&str]) {
            assert!(!docs.is_empty(), "{} has no docs", M::NAME);
            self.0.push(M::NAME);
        }

        fn notification<N: NotificationMethod>(&mut self, docs: &[&str]) {
            assert!(!docs.is_empty(), "{} has no docs", N::NAME);
            self.0.push(N::NAME);
        }
    }

    #[test]
    fn the_table_has_every_m1_method_once() {
        let mut names = Names::default();
        visit(&mut names);
        assert_eq!(
            names.0,
            [
                "initialize",
                "host/health",
                "host/version",
                "project/list",
                "project/create",
                "events/subscribe",
                "events/unsubscribe",
                "$/cancelRequest",
                "events/event",
            ]
        );
        assert_eq!(names.0.iter().collect::<BTreeSet<_>>().len(), names.0.len());
    }
}
