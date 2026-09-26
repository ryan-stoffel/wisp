//! The event log: every change on this host, numbered by a daemon-wide `seq` (0007).
//!
//! From M3 the log lives in SQLite (#156, decision 0014): [`EventLog::open`] keeps its own
//! connection to the store's database, writes every event there as it is appended, and on start
//! reloads the log's id, its head `seq`, and the newest events. So `logId` and `seq` survive a
//! restart, and `agent/events` can page through a run's whole history. The newest `retention`
//! events are also kept in memory, which is what `events/subscribe` replays from; older ones need
//! a resync. If the database can't be opened, the log runs in memory only, starting over with a
//! new `logId` on every start, as it did in M1.
//!
//! The table is compacted on a retention policy (#187, decision 0016): an agent run's events stay
//! as long as its run row does (nothing removes one yet, so in practice they are not pruned by
//! this log), while host and project events with no `run_id` — `project.created`,
//! `context.changed` — are pruned to the newest `host_retention` after each one is appended. The
//! in-memory window that `events/subscribe` replays from only ever shrinks by eviction here, never
//! by a database read, so that pruning can't change what a live subscriber sees or make
//! `resyncRequired` fire incorrectly; it only shortens what a restart can reload. That in-memory
//! window is itself bounded by both count (`retention`) and bytes (`max_bytes`), evicting from the
//! front once either is exceeded, so a run with many large `agent.output` batches can't hold
//! unbounded memory.
//!
//! Subscribers don't get their own queues. Each subscription is a cursor that reads the log (see
//! `methods::events::Cursors`), so a slow subscriber costs nothing until it reads, and whoever
//! appends never waits for one.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use jiff::Timestamp;
use tokio::sync::watch;
use tracing::{error, info, warn};
use uuid::Uuid;
use wisp_protocol::{LogId, ProjectId, RunId, WispEvent};
use wisp_store::{Store, StoreError, StoredEvent};

/// One entry in the log.
#[derive(Debug)]
pub(crate) struct Entry {
    pub seq: u64,
    pub time: Timestamp,
    pub project: Option<ProjectId>,
    pub event: WispEvent,
    /// The event's JSON size, for the in-memory replay window's byte bound.
    bytes: usize,
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
    /// The in-memory replay window's byte bound (#187): even within `retention`, evicts older
    /// events once their JSON exceeds this many bytes.
    max_bytes: usize,
    /// How many of the newest host and project events (no `run_id`) the stored log keeps;
    /// older ones are pruned after each one is appended. Irrelevant for a log with no database.
    host_retention: usize,
    inner: Mutex<Inner>,
    head: watch::Sender<u64>,
}

struct Inner {
    events: VecDeque<Arc<Entry>>,
    /// The sum of `events`' sizes, kept alongside for O(1) eviction decisions.
    bytes: usize,
    /// The log's own connection to the store's database, or `None` for a log in memory only.
    db: Option<Store>,
}

/// The run an event belongs to, for `agent/events`.
pub(crate) fn run_of(event: &WispEvent) -> Option<RunId> {
    match event {
        WispEvent::AgentStarted { run_id, .. }
        | WispEvent::AgentUpdated { run_id, .. }
        | WispEvent::AgentOutput { run_id, .. }
        | WispEvent::AgentAccountFallback { run_id, .. }
        | WispEvent::AgentFinished { run_id, .. }
        | WispEvent::AgentDiffReady { run_id, .. } => Some(*run_id),
        WispEvent::ProjectCreated { .. }
        | WispEvent::ContextChanged { .. }
        | WispEvent::Unknown => None,
    }
}

fn kind_of(event: &WispEvent) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|value| value.get("kind")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

impl EventLog {
    /// An empty log in memory only, which keeps the newest `retention` events with no byte bound.
    /// For tests that don't care about the byte bound or the store's own host-event pruning.
    #[cfg(test)]
    pub fn new(retention: usize) -> Self {
        Self::new_bounded(retention, usize::MAX)
    }

    /// An empty log in memory only, which keeps the newest `retention` events and evicts once
    /// their JSON passes `max_bytes`, whichever comes first.
    pub fn new_bounded(retention: usize, max_bytes: usize) -> Self {
        Self::with(
            LogId::generate(),
            retention,
            max_bytes,
            usize::MAX,
            VecDeque::new(),
            0,
            None,
        )
    }

    /// The log stored in the database at `path`, with the newest `retention` events in memory
    /// (evicting sooner if they pass `max_bytes`), and the newest `host_retention` host and
    /// project events kept in the table. Falls back to a log in memory only if the database can't
    /// be opened or read.
    pub fn open(path: &Path, retention: usize, max_bytes: usize, host_retention: usize) -> Self {
        match Self::load(path, retention, max_bytes, host_retention) {
            Ok(log) => log,
            Err(error) => {
                error!(path = %path.display(), %error, "could not open the event log's database; keeping it in memory only");
                Self::new_bounded(retention, max_bytes)
            }
        }
    }

    fn load(
        path: &Path,
        retention: usize,
        max_bytes: usize,
        host_retention: usize,
    ) -> Result<Self, StoreError> {
        let db = Store::open(path)?;
        db.relax_sync()?;
        let stored_id = db.event_log_id(LogId::generate().into())?;
        let id = LogId::try_from(stored_id).unwrap_or_else(|_| {
            warn!(id = %stored_id, "the stored event log id is not a UUIDv7");
            LogId::generate()
        });
        let head = db.event_head()?;
        let events = db
            .latest_events(retention.max(1))?
            .into_iter()
            .map(|stored| Arc::new(entry(&stored)))
            .collect();
        info!(log_id = %id, head, "opened the event log");
        Ok(Self::with(
            id,
            retention,
            max_bytes,
            host_retention,
            events,
            head,
            Some(db),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    fn with(
        id: LogId,
        retention: usize,
        max_bytes: usize,
        host_retention: usize,
        events: VecDeque<Arc<Entry>>,
        head: u64,
        db: Option<Store>,
    ) -> Self {
        let bytes = events.iter().map(|entry| entry.bytes).sum();
        Self {
            id,
            retention: retention.max(1),
            max_bytes: max_bytes.max(1),
            host_retention: host_retention.max(1),
            inner: Mutex::new(Inner { events, bytes, db }),
            head: watch::Sender::new(head),
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

    /// Appends an event, stores it, and returns its `seq`. An event that can't be stored is still
    /// delivered from memory, and the failure is logged.
    pub fn append(&self, time: Timestamp, project: Option<ProjectId>, event: WispEvent) -> u64 {
        let mut inner = self.inner();
        let seq = self.head() + 1;
        let run_id = run_of(&event);
        let payload = serde_json::to_string(&event).unwrap_or_default();
        if let Some(db) = &inner.db {
            let stored = StoredEvent {
                seq,
                time,
                project_id: project.map(Uuid::from),
                run_id: run_id.map(Uuid::from),
                kind: kind_of(&event),
                payload: payload.clone(),
            };
            if let Err(error) = db.append_event(&stored) {
                error!(seq, %error, "could not store an event; it is delivered but not kept");
                // The stored log now has a hole, and a later wispd could give this `seq` out
                // again. A new `logId` on the next start makes every client resync instead.
                if let Err(error) = db.reset_event_log_id() {
                    error!(%error, "could not mark the event log to start over");
                }
            } else if run_id.is_none() {
                // Only a host or project event can grow past the retention this way (#187): a
                // run's events stay until its run row is removed, which nothing does yet.
                if let Err(error) = db.prune_host_events(self.host_retention) {
                    error!(%error, "could not prune host and project events");
                }
            }
        }
        let bytes = payload.len();
        inner.events.push_back(Arc::new(Entry {
            seq,
            time,
            project,
            event,
            bytes,
        }));
        inner.bytes += bytes;
        // Always keep at least the event just appended, even if it alone passes `max_bytes`.
        while inner.events.len() > 1
            && (inner.events.len() > self.retention || inner.bytes > self.max_bytes)
        {
            if let Some(evicted) = inner.events.pop_front() {
                inner.bytes = inner.bytes.saturating_sub(evicted.bytes);
            }
        }
        self.head.send_replace(seq);
        seq
    }

    /// `run`'s events after `after`, oldest first, from the database, or from memory for a log
    /// that has none: at most `limit` of them and about `max_bytes` of event JSON, but always at
    /// least one when any exists, so a page fits in a frame and paging always moves on. The
    /// flag says whether more follow.
    pub fn run_events(
        &self,
        run: RunId,
        after: u64,
        limit: usize,
        max_bytes: usize,
    ) -> Result<(Vec<Arc<Entry>>, bool), StoreError> {
        let inner = self.inner();
        if let Some(db) = &inner.db {
            let (stored, more) = db.run_events(run.into(), after, limit, max_bytes)?;
            let entries = stored
                .iter()
                .map(|stored| Arc::new(entry(stored)))
                .collect();
            return Ok((entries, more));
        }
        let mut entries = Vec::new();
        let mut bytes = 0;
        for entry in inner
            .events
            .iter()
            .filter(|entry| entry.seq > after && run_of(&entry.event) == Some(run))
        {
            let size = serde_json::to_string(&entry.event).map_or(0, |json| json.len());
            if entries.len() >= limit.max(1) || (!entries.is_empty() && bytes + size > max_bytes) {
                return Ok((entries, true));
            }
            bytes += size;
            entries.push(Arc::clone(entry));
        }
        Ok((entries, false))
    }

    /// Whether the events after `after` can all still be replayed.
    pub fn check(&self, after: u64) -> Result<(), Gone> {
        self.start(&self.inner().events, after).map(|_| ())
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
        let inner = self.inner();
        let start = self.start(&inner.events, after)?;
        match inner
            .events
            .range(start..)
            .find(|event| event.project == project)
        {
            Some(event) => Ok((Some(Arc::clone(event)), event.seq)),
            None => Ok((None, self.head())),
        }
    }

    // The index of the first event after `after`. `seq`s increase but may have gaps: an event
    // that failed to be stored is missing from a log reloaded after a restart.
    fn start(&self, events: &VecDeque<Arc<Entry>>, after: u64) -> Result<usize, Gone> {
        let head = self.head();
        if after > head {
            return Err(Gone::Unknown { head });
        }
        let oldest = events.front().map_or(head + 1, |event| event.seq);
        if after + 1 < oldest {
            return Err(Gone::Dropped);
        }
        Ok(events.partition_point(|event| event.seq <= after))
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A stored event as a log entry. A payload this build can't read, such as a newer wispd's kind,
/// comes back as `WispEvent::Unknown`, keeping its place in the sequence.
fn entry(stored: &StoredEvent) -> Entry {
    Entry {
        seq: stored.seq,
        time: stored.time,
        project: stored
            .project_id
            .and_then(|id| ProjectId::try_from(id).ok()),
        bytes: stored.payload.len(),
        event: serde_json::from_str(&stored.payload).unwrap_or(WispEvent::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use wisp_protocol::{AgentOutcome, AgentOutputItem, ProjectId, RunId, WispEvent};

    use super::{EventLog, Gone};

    fn append(log: &EventLog, project: Option<ProjectId>) -> u64 {
        log.append(jiff::Timestamp::now(), project, WispEvent::Unknown)
    }

    fn finished(run_id: RunId) -> WispEvent {
        WispEvent::AgentFinished {
            run_id,
            outcome: AgentOutcome::Cancelled,
        }
    }

    /// A stored log with no byte bound and no host-event pruning, for tests that only care about
    /// `retention`.
    fn open(path: &Path, retention: usize) -> EventLog {
        EventLog::open(path, retention, usize::MAX, usize::MAX)
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
    fn a_stored_log_keeps_its_id_seq_and_events_across_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wispd.sqlite3");
        let project = ProjectId::generate();
        let run = RunId::generate();
        let first = open(&path, 2);
        append(&first, None);
        first.append(jiff::Timestamp::now(), Some(project), finished(run));
        append(&first, Some(project));
        let id = first.id();
        drop(first);

        let reopened = open(&path, 2);
        assert_eq!(reopened.id(), id, "the log did not start over");
        assert_eq!(reopened.head(), 3);
        assert_eq!(
            reopened.check(0),
            Err(Gone::Dropped),
            "only 2 are in memory"
        );
        let (event, _) = reopened.next(1, Some(project)).unwrap();
        assert_eq!(event.unwrap().event, finished(run));
        assert_eq!(append(&reopened, None), 4);
        let (entries, more) = reopened.run_events(run, 0, 10, usize::MAX).unwrap();
        let seqs: Vec<u64> = entries.iter().map(|entry| entry.seq).collect();
        assert_eq!(seqs, [2]);
        assert!(!more);
        assert!(
            reopened
                .run_events(run, 2, 10, usize::MAX)
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[test]
    fn a_log_whose_database_cannot_open_runs_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file");
        std::fs::write(&file, "").unwrap();
        let log = open(&file.join("nested.sqlite3"), 10);
        let run = RunId::generate();
        log.append(jiff::Timestamp::now(), None, finished(run));
        assert_eq!(log.head(), 1);
        log.append(jiff::Timestamp::now(), None, finished(run));
        assert_eq!(log.run_events(run, 0, 10, usize::MAX).unwrap().0.len(), 2);
        let (page, more) = log.run_events(run, 0, 10, 1).unwrap();
        assert_eq!(
            (page.len(), more),
            (1, true),
            "the byte budget applies in memory too"
        );
    }

    #[test]
    fn replay_skips_nothing_across_a_gap_in_seq() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wispd.sqlite3");
        let first = open(&path, 10);
        for _ in 0..4 {
            append(&first, None);
        }
        let id = first.id();
        drop(first);
        // An event that was never stored leaves a hole, as a failed insert would.
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute("DELETE FROM events WHERE seq = 2", []).unwrap();
        drop(db);

        let reopened = open(&path, 10);
        assert_eq!(reopened.id(), id);
        let mut delivered = Vec::new();
        let mut after = 0;
        while let (Some(entry), seq) = reopened.next(after, None).unwrap() {
            delivered.push(entry.seq);
            after = seq;
        }
        assert_eq!(delivered, [1, 3, 4]);
        assert_eq!(reopened.next(2, None).unwrap().0.unwrap().seq, 3);
    }

    #[test]
    fn a_failed_insert_gives_the_log_a_new_id_on_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wispd.sqlite3");
        let log = open(&path, 10);
        append(&log, None);
        let id = log.id();
        // A row already holding the next `seq` makes the insert fail.
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute(
            "INSERT INTO events (seq, time, kind, payload) VALUES (2, '2026-09-25T12:00:00Z', 'x', '{}')",
            [],
        )
        .unwrap();
        drop(db);
        assert_eq!(append(&log, None), 2, "still delivered");
        drop(log);
        assert_ne!(open(&path, 10).id(), id);
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

    fn big_output(run_id: RunId, text: &str) -> WispEvent {
        WispEvent::AgentOutput {
            run_id,
            items: vec![AgentOutputItem::TextDelta {
                message_id: None,
                text: text.to_owned(),
            }],
        }
    }

    #[test]
    fn the_in_memory_window_evicts_on_bytes_before_it_would_on_count() {
        let run = RunId::generate();
        let one = serde_json::to_string(&big_output(run, &"a".repeat(80)))
            .unwrap()
            .len();
        // Room for a little more than one event, so a second one evicts the first even though
        // `retention` (1000) is nowhere close.
        let log = EventLog::new_bounded(1000, one + 10);
        log.append(
            jiff::Timestamp::now(),
            None,
            big_output(run, &"a".repeat(80)),
        );
        log.append(
            jiff::Timestamp::now(),
            None,
            big_output(run, &"b".repeat(80)),
        );
        log.append(
            jiff::Timestamp::now(),
            None,
            big_output(run, &"c".repeat(80)),
        );

        assert_eq!(
            log.check(0),
            Err(Gone::Dropped),
            "the byte bound evicted seq 1 well before the count bound would"
        );
        assert_eq!(log.check(2), Ok(()));
        assert_eq!(log.next(2, None).unwrap().0.unwrap().seq, 3);
    }

    #[test]
    fn stored_host_events_are_pruned_but_run_events_and_live_replay_stay_correct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wispd.sqlite3");
        let run = RunId::generate();
        let log = EventLog::open(&path, 10, usize::MAX, 1);
        append(&log, None); // seq 1: host, pruned once seq 3 lands
        log.append(jiff::Timestamp::now(), None, finished(run)); // seq 2: run, never pruned
        append(&log, None); // seq 3: host, prunes seq 1
        append(&log, None); // seq 4: host, prunes seq 3

        // The in-memory window still has everything (`retention` is 10), so a live subscriber
        // sees every event; pruning is a database-only, restart-only concern.
        assert_eq!(log.check(0), Ok(()));
        assert_eq!(log.next(0, None).unwrap().0.unwrap().seq, 1);
        let id = log.id();
        drop(log);

        let reopened = EventLog::open(&path, 10, usize::MAX, 1);
        assert_eq!(reopened.id(), id, "the log did not start over");
        assert_eq!(
            reopened.head(),
            4,
            "seq keeps counting the pruned events too"
        );
        assert_eq!(
            reopened.check(0),
            Err(Gone::Dropped),
            "resyncRequired still fires correctly: seq 1 was really pruned"
        );
        let mut seqs = Vec::new();
        let mut after = 1;
        while let (Some(entry), seq) = reopened.next(after, None).unwrap() {
            seqs.push(entry.seq);
            after = seq;
        }
        assert_eq!(
            seqs,
            [2, 4],
            "seq 1 and 3 were pruned from the table; the run event and the newest host event \
             are what a restart can still reload"
        );
        let (run_entries, more) = reopened.run_events(run, 0, 10, usize::MAX).unwrap();
        assert_eq!(
            run_entries
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            [2],
            "agent/events still finds the run's only event"
        );
        assert!(!more);
    }
}
