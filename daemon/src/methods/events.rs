//! `events/subscribe` and `events/unsubscribe`, and the cursors that deliver a connection's
//! events.

use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    ErrorKind, EventsEventParams, EventsSubscribeParams, ProjectId, SubscriptionId,
};

use super::Context;
use crate::event_log::{EventLog, Gone};
use crate::store::store_error;

/// One subscription's place in the event log.
#[derive(Debug)]
pub(crate) struct Cursor {
    pub subscription: SubscriptionId,
    /// The project whose events it gets, or `None` for host-level events.
    pub project: Option<ProjectId>,
    /// The `seq` of the last event delivered or skipped.
    pub after: u64,
}

/// Checks a subscription and returns its cursor. The connection's writer sends the response and
/// then starts delivering from the cursor, so no event can arrive before the response.
pub(crate) async fn subscribe(
    context: &Context,
    params: EventsSubscribeParams,
) -> Result<Cursor, ErrorObject> {
    if let Some(project) = params.project {
        let exists = context
            .daemon
            .store
            .run(&context.cancel, move |store| {
                store
                    .get_project(project.into())
                    .map(|row| row.is_some())
                    .map_err(|error| store_error(&error))
            })
            .await?;
        if !exists {
            return Err(ErrorObject::wisp(
                ErrorKind::ProjectNotFound,
                format!("no project has id {project}"),
            ));
        }
    }
    context
        .daemon
        .log
        .check(params.after)
        .map_err(|gone| resync_required(params.after, gone))?;
    Ok(Cursor {
        subscription: SubscriptionId::generate(),
        project: params.project,
        after: params.after,
    })
}

fn resync_required(after: u64, gone: Gone) -> ErrorObject {
    let message = match gone {
        Gone::Dropped => format!("the events after seq {after} are no longer available"),
        Gone::Unknown { head } => {
            format!("seq {after} is not in this event log, whose last event is {head}")
        }
    };
    ErrorObject::wisp(ErrorKind::ResyncRequired, message)
}

/// A connection's subscriptions.
#[derive(Debug, Default)]
pub(crate) struct Cursors {
    cursors: Vec<Cursor>,
    turn: usize,
}

impl Cursors {
    pub fn add(&mut self, cursor: Cursor) {
        self.cursors.push(cursor);
    }

    /// Ends a subscription. Ending one that doesn't exist does nothing.
    pub fn remove(&mut self, subscription: SubscriptionId) {
        self.cursors
            .retain(|cursor| cursor.subscription != subscription);
    }

    /// The next event for one of the subscriptions, which take turns so one busy project can't
    /// hold up another.
    ///
    /// # Errors
    ///
    /// The subscription whose next events the log no longer has.
    pub fn next(&mut self, log: &EventLog) -> Result<Option<EventsEventParams>, SubscriptionId> {
        for _ in 0..self.cursors.len() {
            let index = self.turn % self.cursors.len();
            self.turn = self.turn.wrapping_add(1);
            let cursor = &mut self.cursors[index];
            let (event, seq) = log
                .next(cursor.after, cursor.project)
                .map_err(|_| cursor.subscription)?;
            cursor.after = seq;
            if let Some(event) = event {
                return Ok(Some(EventsEventParams {
                    subscription: cursor.subscription,
                    seq: event.seq,
                    time: event.time,
                    project: event.project,
                    event: event.event.clone(),
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use wisp_protocol::{ProjectId, SubscriptionId, WispEvent};

    use super::{Cursor, Cursors};
    use crate::event_log::EventLog;

    fn cursor(project: Option<ProjectId>, after: u64) -> Cursor {
        Cursor {
            subscription: SubscriptionId::generate(),
            project,
            after,
        }
    }

    fn drain(cursors: &mut Cursors, log: &EventLog) -> Vec<(SubscriptionId, u64)> {
        let mut delivered = Vec::new();
        while let Some(event) = cursors.next(log).unwrap() {
            delivered.push((event.subscription, event.seq));
        }
        delivered
    }

    #[test]
    fn each_subscription_gets_its_own_events_after_its_seq() {
        let log = EventLog::new(10);
        let project = ProjectId::generate();
        for owner in [None, Some(project), None] {
            log.append_blocking(Timestamp::now(), owner, WispEvent::Unknown);
        }
        let host = cursor(None, 0);
        let (host_id, late_id) = (host.subscription, SubscriptionId::generate());
        let project_cursor = cursor(Some(project), 0);
        let project_id = project_cursor.subscription;
        let mut cursors = Cursors::default();
        cursors.add(host);
        cursors.add(project_cursor);
        cursors.add(Cursor {
            subscription: late_id,
            project: None,
            after: 1,
        });

        let mut delivered = drain(&mut cursors, &log);
        delivered.sort_by_key(|&(subscription, seq)| (seq, subscription));
        let mut expected = vec![(host_id, 1), (project_id, 2), (host_id, 3), (late_id, 3)];
        expected.sort_by_key(|&(subscription, seq)| (seq, subscription));
        assert_eq!(delivered, expected);

        log.append_blocking(Timestamp::now(), None, WispEvent::Unknown);
        cursors.remove(host_id);
        assert_eq!(drain(&mut cursors, &log), [(late_id, 4)]);
    }

    #[test]
    fn a_cursor_behind_the_retention_is_reported() {
        let log = EventLog::new(1);
        log.append_blocking(Timestamp::now(), None, WispEvent::Unknown);
        log.append_blocking(Timestamp::now(), None, WispEvent::Unknown);
        let lagging = cursor(None, 0);
        let id = lagging.subscription;
        let mut cursors = Cursors::default();
        cursors.add(lagging);
        assert_eq!(cursors.next(&log).unwrap_err(), id);
    }
}
