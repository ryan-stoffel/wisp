//! The event log: every change on this host, numbered by a daemon-wide `seq` (0007).
//!
//! In M1 the log lives in memory. It starts over with a new `logId` whenever wispd starts, and it
//! keeps only the newest events. M3 moves it into SQLite behind the same methods.
//!
//! Subscribers don't get their own queues. Each subscription is a cursor that reads the log (see
//! `methods::events::Cursors`), so a slow subscriber costs nothing until it reads, and whoever
//! appends never waits for one.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jiff::Timestamp;
use tokio::sync::watch;
use wisp_protocol::{LogId, ProjectId, WispEvent};

/// One entry in the log.
#[derive(Debug)]
pub(crate) struct Entry {
    pub seq: u64,
    pub time: Timestamp,
    pub project: Option<ProjectId>,
    pub event: WispEvent,
}

/// Why the events after a `seq` can't be replayed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Gone {
    /// Some of them were dropped to keep the log within its retention.
    Dropped,
    /// The `seq` is past the newest event, so it came from another log.
    Unknown {
        /// The newest event's `seq`.
        head: u64,
    },
}

pub(crate) struct EventLog {
    id: LogId,
    retention: usize,
    events: Mutex<VecDeque<Arc<Entry>>>,
    head: watch::Sender<u64>,
}

impl EventLog {
    /// An empty log that keeps the newest `retention` events.
    pub fn new(retention: usize) -> Self {
        Self {
            id: LogId::generate(),
            retention: retention.max(1),
            events: Mutex::new(VecDeque::new()),
            head: watch::Sender::new(0),
        }
    }

    pub fn id(&self) -> LogId {
        self.id
    }

    /// The newest event's `seq`, or 0 before the first.
    pub fn head(&self) -> u64 {
        *self.head.borrow()
    }

    /// A receiver that wakes after every append.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.head.subscribe()
    }

    /// Appends an event and returns its `seq`.
    pub fn append(&self, time: Timestamp, project: Option<ProjectId>, event: WispEvent) -> u64 {
        let mut events = self.events();
        let seq = self.head() + 1;
        events.push_back(Arc::new(Entry {
            seq,
            time,
            project,
            event,
        }));
        while events.len() > self.retention {
            events.pop_front();
        }
        self.head.send_replace(seq);
        seq
    }

    /// Whether the events after `after` can all still be replayed.
    pub fn check(&self, after: u64) -> Result<(), Gone> {
        self.start(&self.events(), after).map(|_| ())
    }

    /// The first event after `after` that belongs to `project`, where `None` means host-level
    /// events.
    ///
    /// The `seq` returned with it is where to continue from: the event's own, or the head's when
    /// no such event exists yet.
    pub fn next(
        &self,
        after: u64,
        project: Option<ProjectId>,
    ) -> Result<(Option<Arc<Entry>>, u64), Gone> {
        let events = self.events();
        let start = self.start(&events, after)?;
        match events.range(start..).find(|event| event.project == project) {
            Some(event) => Ok((Some(Arc::clone(event)), event.seq)),
            None => Ok((None, self.head())),
        }
    }

    // The index of the first event after `after`.
    fn start(&self, events: &VecDeque<Arc<Entry>>, after: u64) -> Result<usize, Gone> {
        let head = self.head();
        if after > head {
            return Err(Gone::Unknown { head });
        }
        let oldest = events.front().map_or(head + 1, |event| event.seq);
        if after + 1 < oldest {
            return Err(Gone::Dropped);
        }
        usize::try_from(after + 1 - oldest).map_err(|_| Gone::Dropped)
    }

    fn events(&self) -> MutexGuard<'_, VecDeque<Arc<Entry>>> {
        self.events.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use wisp_protocol::{ProjectId, WispEvent};

    use super::{EventLog, Gone};

    fn append(log: &EventLog, project: Option<ProjectId>) -> u64 {
        log.append(jiff::Timestamp::now(), project, WispEvent::Unknown)
    }

    #[test]
    fn seq_starts_at_1_and_counts_every_event() {
        let log = EventLog::new(10);
        assert_eq!(log.head(), 0);
        assert_eq!(append(&log, None), 1);
        assert_eq!(append(&log, Some(ProjectId::generate())), 2);
        assert_eq!(log.head(), 2);
    }

    #[test]
    fn next_skips_other_projects_and_reports_where_it_stopped() {
        let log = EventLog::new(10);
        let project = ProjectId::generate();
        append(&log, Some(project));
        append(&log, None);
        append(&log, Some(project));

        let (event, seq) = log.next(0, None).unwrap();
        assert_eq!(event.unwrap().seq, 2);
        assert_eq!(seq, 2);
        assert_eq!(log.next(2, None).unwrap().0.map(|event| event.seq), None);
        assert_eq!(log.next(2, None).unwrap().1, 3);

        let (event, _) = log.next(1, Some(project)).unwrap();
        assert_eq!(event.unwrap().seq, 3);
        assert_eq!(log.next(3, Some(project)).unwrap().1, 3);
    }

    #[test]
    fn events_past_the_retention_are_gone() {
        let log = EventLog::new(2);
        for _ in 0..4 {
            append(&log, None);
        }
        assert_eq!(log.check(1), Err(Gone::Dropped));
        assert_eq!(log.next(1, None).unwrap_err(), Gone::Dropped);
        assert_eq!(log.check(2), Ok(()));
        assert_eq!(log.next(2, None).unwrap().0.unwrap().seq, 3);
        assert_eq!(log.check(4), Ok(()));
    }

    #[test]
    fn a_seq_past_the_head_is_unknown() {
        let log = EventLog::new(2);
        assert_eq!(log.check(0), Ok(()));
        assert_eq!(log.check(1), Err(Gone::Unknown { head: 0 }));
        append(&log, None);
        assert_eq!(log.check(u64::MAX), Err(Gone::Unknown { head: 1 }));
    }

    #[test]
    fn appends_wake_watchers() {
        let log = EventLog::new(2);
        let mut watcher = log.watch();
        assert!(!watcher.has_changed().unwrap());
        append(&log, None);
        assert!(watcher.has_changed().unwrap());
        assert_eq!(*watcher.borrow_and_update(), 1);
    }
}
