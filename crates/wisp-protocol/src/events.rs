use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::id::uuid_v7_id;
use crate::{Project, ProjectId};

uuid_v7_id! {
    /// Identifies one `events/subscribe` on one connection. wispd generates it.
    SubscriptionId
}

uuid_v7_id! {
    /// Identifies wispd's event log. It changes only when the log starts over.
    LogId
}

/// Params of `events/subscribe`.
///
/// wispd replays the events after `after`, then sends new ones as they happen, each as an
/// `events/event` notification. If those events are gone or too many to replay, it fails with
/// `resyncRequired`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EventsSubscribeParams {
    /// Replay the events whose `seq` is greater than this: usually the `seq` of a snapshot, such
    /// as `project/list`'s, or of the last event received. `seq` starts at 1.
    pub after: u64,
    /// Subscribe to one project's events. Without it, the subscription gets host-level events,
    /// such as `project.created`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project: Option<ProjectId>,
}

/// Result of `events/subscribe`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EventsSubscribeResult {
    /// The new subscription, which its `events/event` notifications name.
    pub subscription: SubscriptionId,
}

/// Params of `events/unsubscribe`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EventsUnsubscribeParams {
    /// The subscription to end. Ending one that does not exist succeeds.
    pub subscription: SubscriptionId,
}

/// Result of `events/unsubscribe`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EventsUnsubscribeResult {}

/// Params of `events/event`: one event from wispd's event log.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct EventsEventParams {
    /// The subscription it belongs to.
    pub subscription: SubscriptionId,
    /// The event's position in wispd's log. It increases by one for every change on the host,
    /// across all projects.
    pub seq: u64,
    /// When the change happened, in RFC 3339 UTC.
    pub time: Timestamp,
    /// The project it belongs to. Absent for host-level events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub project: Option<ProjectId>,
    /// What happened.
    pub event: WispEvent,
}

/// What happened, by `kind`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum WispEvent {
    /// A project was created. Host-level.
    #[serde(rename = "project.created")]
    ProjectCreated {
        /// The new project.
        project: Project,
    },
    /// A kind this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{EventsEventParams, EventsSubscribeParams, WispEvent};

    #[test]
    fn unknown_event_kinds_decode_as_unknown() {
        let params: EventsEventParams = serde_json::from_value(json!({
            "subscription": "01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e7f",
            "seq": 9,
            "time": "2026-09-24T12:00:00Z",
            "project": "01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e80",
            "event": {"kind": "agent.started", "runId": "01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e81"}
        }))
        .unwrap();
        assert_eq!(params.event, WispEvent::Unknown);
        assert!(params.project.is_some());
    }

    #[test]
    fn project_is_omitted_when_absent() {
        let params = EventsSubscribeParams {
            after: 0,
            project: None,
        };
        assert_eq!(serde_json::to_value(params).unwrap(), json!({"after": 0}));
    }

    #[test]
    fn times_are_normalized_to_utc() {
        let params: EventsEventParams = serde_json::from_value(json!({
            "subscription": "01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e7f",
            "seq": 1,
            "time": "2026-09-24T14:00:00.250+02:00",
            "event": {"kind": "agent.started"}
        }))
        .unwrap();
        assert_eq!(
            serde_json::to_value(params.time).unwrap(),
            json!("2026-09-24T12:00:00.25Z")
        );
        assert!(
            serde_json::from_value::<jiff::Timestamp>(json!("2026-09-24T12:00:00")).is_err(),
            "a time without an offset is ambiguous"
        );
    }
}
