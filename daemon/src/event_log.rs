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
//! `context.changed` — are pruned to the newest `host_retention` after each one is appended.
//! `host_retention` is always at least `retention` (`EventLog::with` enforces it): a restart only
//! ever reloads the newest `retention` events, and by pigeonhole every host or project event in
//! that reload is among the newest `retention` host and project events too, so keeping at least
//! that many never lets pruning remove one a reload still needs. A live subscriber is unaffected
//! either way, since `events/subscribe`'s replay (`check`/`next`) only ever reads the in-memory
//! window, never the database; pruning only changes what a later restart can reload, and the
//! invariant keeps that reload gap-free rather than merely shorter. That in-memory window is
//! itself bounded by both count (`retention`) and bytes (`max_bytes`), evicting from the front
//! once either is exceeded — on every append, and once more on load in case a restart's reload
//! didn't already fit — so a run with many large `agent.output` batches can't hold unbounded
//! memory even right after a restart.
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
    /// The oldest `seq` that can still be replayed. Eviction moves it on; [`EventLog::purge_run`]
    /// doesn't, since the events it removes are gone on purpose, not dropped.
    floor: u64,
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
        | WispEvent::AgentDiffReady { run_id, .. }
        | WispEvent::AgentAccepted { run_id, .. } => Some(*run_id),
        WispEvent::ProjectCreated { .. }
        | WispEvent::ContextChanged { .. }
        | WispEvent::RepoAdded { .. }
        | WispEvent::ThreadStarted { .. }
        | WispEvent::ThreadUpdated { .. }
        | WispEvent::ThreadDeleted { .. }
        | WispEvent::Unknown => None,
    }
}

fn kind_of(event: &WispEvent) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|value| value.get("kind")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Evicts from the front of `events` until it is within both `retention` and `max_bytes`,
/// keeping `bytes` (the sum of what remains) in sync. Always leaves at least one event, so a
/// single one over `max_bytes` on its own is never dropped outright. Shared by construction and
/// by every append, so the bound holds the same way whichever put the log over it.
fn evict(
    events: &mut VecDeque<Arc<Entry>>,
    bytes: &mut usize,
    floor: &mut u64,
    retention: usize,
    max_bytes: usize,
) {
    while events.len() > 1 && (events.len() > retention || *bytes > max_bytes) {
        if let Some(evicted) = events.pop_front() {
            *bytes = bytes.saturating_sub(evicted.bytes);
            *floor = evicted.seq + 1;
        }
    }
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
            .latest_events(retention.max(1), max_bytes)?
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
        mut events: VecDeque<Arc<Entry>>,
        head: u64,
        db: Option<Store>,
    ) -> Self {
        let retention = retention.max(1);
        let max_bytes = max_bytes.max(1);
        // A restart can only reload what's in the table already, so `latest_events` applies both
        // bounds itself; this is a second, defensive pass in case `events` didn't (`new_bounded`
        // starts empty, so it's a no-op there). `host_retention` must be at least `retention`: a
        // reload only ever pulls the newest `retention` events, and every host or project event
        // among them is, by pigeonhole, among the newest `retention` host and project events, so
        // keeping at least that many host events never lets a restart's window skip one. A
        // smaller `host_retention` could prune a host event that a reload still expects, leaving
        // a hole `resyncRequired` would never notice (0016).
        let host_retention = host_retention.max(retention);
        let mut bytes = events.iter().map(|entry| entry.bytes).sum();
        let mut floor = events.front().map_or(head + 1, |entry| entry.seq);
        evict(&mut events, &mut bytes, &mut floor, retention, max_bytes);
        Self {
            id,
            retention,
            max_bytes,
            host_retention,
            inner: Mutex::new(Inner {
                events,
                bytes,
                floor,
                db,
            }),
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
        let bytes = payload.len();
        if let Some(db) = &inner.db {
            let stored = StoredEvent {
                seq,
                time,
                project_id: project.map(Uuid::from),
                run_id: run_id.map(Uuid::from),
                kind: kind_of(&event),
                payload,
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
        inner.events.push_back(Arc::new(Entry {
            seq,
            time,
            project,
            event,
            bytes,
        }));
        inner.bytes += bytes;
        let Inner {
            events: entries,
            bytes: total_bytes,
            floor,
            ..
        } = &mut *inner;
        evict(entries, total_bytes, floor, self.retention, self.max_bytes);
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

    /// Removes `run`'s events from the in-memory replay window, for a deleted thread (#110), so
    /// `events/subscribe` stops replaying them. The stored ones go with the run's rows.
    pub fn purge_run(&self, run: RunId) {
        let mut inner = self.inner();
        let Inner { events, bytes, .. } = &mut *inner;
        events.retain(|entry| {
            let keep = run_of(&entry.event) != Some(run);
            if !keep {
                *bytes = bytes.saturating_sub(entry.bytes);
            }
            keep
        });
    }

    /// Whether the events after `after` can all still be replayed.
    pub fn check(&self, after: u64) -> Result<(), Gone> {
        self.start(&self.inner(), after).map(|_| ())
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
        let start = self.start(&inner, after)?;
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
    fn start(&self, inner: &Inner, after: u64) -> Result<usize, Gone> {
        let head = self.head();
        if after > head {
            return Err(Gone::Unknown { head });
        }
        if after + 1 < inner.floor {
            return Err(Gone::Dropped);
        }
        Ok(inner.events.partition_point(|event| event.seq <= after))
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
    fn purging_a_run_removes_only_its_events_and_drops_nothing_else() {
        let log = EventLog::new(10);
        let project = ProjectId::generate();
        let (gone, kept) = (RunId::generate(), RunId::generate());
        log.append(jiff::Timestamp::now(), Some(project), finished(gone));
        log.append(jiff::Timestamp::now(), Some(project), finished(kept));
        log.append(jiff::Timestamp::now(), Some(project), finished(gone));

        log.purge_run(gone);

        assert_eq!(log.check(0), Ok(()));
        let (event, seq) = log.next(0, Some(project)).unwrap();
        assert_eq!((event.unwrap().seq, seq), (2, 2));
        let (event, seq) = log.next(2, Some(project)).unwrap();
        assert!(event.is_none());
        assert_eq!(seq, 3);
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
    fn host_retention_below_the_in_memory_retention_is_clamped_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wispd.sqlite3");
        let run = RunId::generate();
        // Ask for host_retention 1 with retention 3: without the clamp in `EventLog::with`,
        // pruning would keep only the single newest host event, taking seq 3 and 4 down with
        // seq 1 even though they're inside what a restart still reloads.
        let log = EventLog::open(&path, 3, usize::MAX, 1);
        append(&log, None); // seq 1: host
        log.append(jiff::Timestamp::now(), None, finished(run)); // seq 2: run, never pruned
        append(&log, None); // seq 3: host
        append(&log, None); // seq 4: host
        append(&log, None); // seq 5: host, old enough that pruning it is legitimate
        let id = log.id();
        drop(log);

        let reopened = EventLog::open(&path, 3, usize::MAX, 1);
        assert_eq!(reopened.id(), id, "the log did not start over");
        assert_eq!(reopened.head(), 5);

        // The clamp made the effective host_retention 3, so only seq 1 (older than every host
        // event the newest-3 window could ever need) was pruned; seq 3 and 4 survived.
        assert_eq!(
            reopened.check(2),
            Ok(()),
            "no silent gap: everything the newest-3 window owes seq 2 is still there"
        );
        let mut seqs = Vec::new();
        let mut after = 2;
        while let (Some(entry), seq) = reopened.next(after, None).unwrap() {
            seqs.push(entry.seq);
            after = seq;
        }
        assert_eq!(seqs, [3, 4, 5], "no hole between the reloaded events");

        // seq 1 is genuinely gone (it's outside the reloaded window entirely), so a client that
        // saw it still correctly needs a resync.
        assert_eq!(reopened.check(0), Err(Gone::Dropped));
    }

    #[test]
    fn reopening_with_a_small_byte_bound_trims_the_reloaded_window() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wispd.sqlite3");
        let run = RunId::generate();
        let one = serde_json::to_string(&big_output(run, &"a".repeat(80)))
            .unwrap()
            .len();
        let log = EventLog::open(&path, 1000, usize::MAX, 1000);
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
        drop(log);

        // `retention` (1000) is nowhere close to 3, so only the byte bound should trim this on
        // reload, before any append ever runs against the reopened log.
        let reopened = EventLog::open(&path, 1000, one + 10, 1000);
        assert_eq!(
            reopened.check(0),
            Err(Gone::Dropped),
            "the byte bound trimmed what was reloaded, not just what a later append would evict"
        );
        assert_eq!(reopened.check(2), Ok(()));
        assert_eq!(reopened.next(2, None).unwrap().0.unwrap().seq, 3);
    }
}
