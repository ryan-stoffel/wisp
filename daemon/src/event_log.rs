//! The event log: every change on this host, numbered by a daemon-wide `seq`.
//!
//! The log lives in SQLite: [`EventLog::open`] opens two connections to the store's database, one
//! owned by a dedicated writer thread, the other kept for `run_events`'s reads, and on start
//! reloads the log's id, its head `seq`, and the newest events, so `logId` and `seq` survive a
//! restart. The newest `retention` events (fewer if they pass `max_bytes`) are also kept in
//! memory, which is what `events/subscribe` replays from; older ones need a resync. If the
//! database can't be opened, the log runs in memory only, with a new `logId` on every start.
//!
//! An agent run's stored events stay as long as its run row does, while host and project events
//! (no `run_id`) are pruned to the newest `host_retention` after each append. `host_retention` is
//! always at least `retention`: a restart only reloads the newest `retention` events, and every
//! host or project event among them is then also among the newest `retention` host and project
//! events, so pruning never removes one a reload still needs.
//!
//! Subscribers don't get their own queues. Each subscription is a cursor that reads the log (see
//! `methods::events::Cursors`), so a slow subscriber costs nothing until it reads, and whoever
//! appends never waits for one.
//!
//! **Locking.** An append is one self-contained job on the [`Writer`] thread that assigns the
//! `seq`, attempts the write, and publishes the entry to the in-memory window. Jobs run one at a
//! time in dispatch order, and once dispatched a job runs to completion even if its caller is
//! dropped mid-await, so the `seq` counter, the window, and SQLite never disagree. A log with no
//! database assigns and publishes under `inner`'s lock with no await in between. `head` lives in
//! [`Inner`] under the same lock as the window, so a reader never sees `head` move before its
//! entry is there; the `watch::Sender` only wakes subscribers to re-check.

use std::collections::VecDeque;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};
use std::thread;

use jiff::Timestamp;
use tokio::sync::{oneshot, watch};
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
    retention: usize,
    /// The in-memory replay window's byte bound: even within `retention`, evicts older events
    /// once their JSON exceeds this many bytes.
    max_bytes: usize,
    /// How many of the newest host and project events (no `run_id`) the stored log keeps;
    /// older ones are pruned after each one is appended. Irrelevant for a log with no database.
    host_retention: usize,
    /// The in-memory replay window and `head`, together, so they're always updated atomically.
    /// `Arc`-wrapped so a write job on the writer thread can outlive its caller.
    inner: Arc<Mutex<Inner>>,
    /// The event log's dedicated writer thread, or `None` for a log with no database.
    writer: Option<Writer>,
    /// A connection dedicated to `run_events`'s reads, so paging a run's history never shares a
    /// lock with appending.
    reader: Option<Mutex<Store>>,
    id: LogId,
    /// Wakes a subscriber to go re-check `inner`; not itself a source of truth for `head`.
    head_watch: watch::Sender<u64>,
}

struct Inner {
    events: VecDeque<Arc<Entry>>,
    /// The sum of `events`' sizes, kept alongside for O(1) eviction decisions.
    bytes: usize,
    /// The newest assigned `seq`, or 0 before the first: the log's real counter.
    head: u64,
    /// The oldest `seq` that can still be replayed. Eviction moves it on; [`EventLog::purge_run`]
    /// doesn't, since the events it removes are gone on purpose, not dropped.
    floor: u64,
}

/// A write to the event log's database, run on [`Writer`]'s own thread.
type WriteJob = Box<dyn FnOnce(&Store) + Send>;

/// Applies writes to the event log's database on a thread of its own, so appending never blocks a
/// tokio worker thread on SQLite. It owns a connection of its own rather than sharing the project
/// store's thread.
struct Writer {
    /// `None` only ever briefly, while `Drop` is closing the channel before it joins the thread.
    jobs: Option<mpsc::Sender<WriteJob>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Writer {
    /// Starts the thread, or fails if it can't be started.
    fn spawn(db: Store) -> std::io::Result<Self> {
        let (jobs, queue) = mpsc::channel::<WriteJob>();
        let thread = thread::Builder::new()
            .name("wispd-events".to_owned())
            .spawn(move || {
                while let Ok(job) = queue.recv() {
                    // A panicking job drops its `done` sender, which only fails that one
                    // caller's wait (see `dispatch`); the thread keeps serving the rest.
                    if catch_unwind(AssertUnwindSafe(|| job(&db))).is_err() {
                        error!("an event log write job panicked");
                    }
                }
            })?;
        Ok(Self {
            jobs: Some(jobs),
            thread: Some(thread),
        })
    }

    /// Runs `job` on the writer thread and waits for its result, from a tokio task: the wait is a
    /// plain `.await`, so it doesn't block the calling worker thread. If the caller's own future
    /// is dropped while waiting, `job` still runs to completion: it was handed off before the
    /// first await point.
    async fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Store) -> T + Send + 'static,
    ) -> Option<T> {
        let (done, wait) = oneshot::channel();
        self.dispatch(job, done);
        wait.await.ok()
    }

    /// Runs `job` on the writer thread and blocks the calling thread until it finishes. Only for
    /// callers with no tokio runtime context of their own: `blocking_recv` panics inside one.
    fn run_blocking<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Store) -> T + Send + 'static,
    ) -> Option<T> {
        let (done, wait) = oneshot::channel();
        self.dispatch(job, done);
        wait.blocking_recv().ok()
    }

    fn dispatch<T: Send + 'static>(
        &self,
        job: impl FnOnce(&Store) -> T + Send + 'static,
        done: oneshot::Sender<T>,
    ) {
        let job: WriteJob = Box::new(move |db| {
            let result = job(db);
            let _ = done.send(result);
        });
        // A send error means the thread ended (or, transiently, is being dropped), which for a
        // running log only happens if it could not be started (jobs never panic past
        // `catch_unwind`); `wait` then resolves on its own once `done` drops, so the caller is
        // never left hanging.
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }
}

impl Drop for Writer {
    /// Closes the job channel first, so the thread's `queue.recv()` loop ends, then joins it, so
    /// shutdown flushes pending writes.
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
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

impl Inner {
    /// Evicts from the front of `events` until it is within both `retention` and `max_bytes`.
    /// Always leaves at least one event, so a single one over `max_bytes` is never dropped.
    fn evict(&mut self, retention: usize, max_bytes: usize) {
        while self.events.len() > 1 && (self.events.len() > retention || self.bytes > max_bytes) {
            if let Some(evicted) = self.events.pop_front() {
                self.bytes = self.bytes.saturating_sub(evicted.bytes);
                self.floor = evicted.seq + 1;
            }
        }
    }

    /// The index of the first event after `after`. `seq`s increase but may have gaps: an event
    /// that failed to be stored is missing from a log reloaded after a restart, and `purge_run`
    /// removes a deleted thread's events from the middle of the window on purpose.
    fn start(&self, after: u64) -> Result<usize, Gone> {
        let head = self.head;
        if after > head {
            return Err(Gone::Unknown { head });
        }
        if after + 1 < self.floor {
            return Err(Gone::Dropped);
        }
        Ok(self.events.partition_point(|event| event.seq <= after))
    }

    /// Pushes `entry` into the window, advances `head` to its `seq`, and notifies watchers, all
    /// under the caller's lock, so no reader sees `head` move before the entry is there.
    fn publish(
        &mut self,
        head_watch: &watch::Sender<u64>,
        retention: usize,
        max_bytes: usize,
        entry: Entry,
    ) {
        let seq = entry.seq;
        self.bytes += entry.bytes;
        self.events.push_back(Arc::new(entry));
        self.head = seq;
        head_watch.send_replace(seq);
        self.evict(retention, max_bytes);
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
    fn new_bounded(retention: usize, max_bytes: usize) -> Self {
        Self::with(
            LogId::generate(),
            retention,
            max_bytes,
            usize::MAX,
            VecDeque::new(),
            0,
            None,
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
        // A writer that can't start fails the whole load, like a database that can't open: keeping
        // the stored `logId` while appending nothing would let a later restart reuse `seq`s this
        // session handed out in memory. The in-memory fallback starts a fresh `logId` instead.
        let writer = Writer::spawn(db)?;
        // WAL mode lets this second connection read alongside the writer thread's.
        let reader = match Store::open(path) {
            Ok(reader) => Some(reader),
            Err(error) => {
                warn!(path = %path.display(), %error, "could not open a second connection for the event log's reads; agent/events falls back to memory");
                None
            }
        };
        info!(log_id = %id, head, "opened the event log");
        Ok(Self::with(
            id,
            retention,
            max_bytes,
            host_retention,
            events,
            head,
            Some(writer),
            reader,
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
        writer: Option<Writer>,
        reader: Option<Store>,
    ) -> Self {
        let retention = retention.max(1);
        let max_bytes = max_bytes.max(1);
        // See the module documentation for why `host_retention` is at least `retention`.
        let host_retention = host_retention.max(retention);
        let mut inner = Inner {
            bytes: events.iter().map(|entry| entry.bytes).sum(),
            floor: events.front().map_or(head + 1, |entry| entry.seq),
            events,
            head,
        };
        // `latest_events` applies both bounds already; this is a defensive second pass.
        inner.evict(retention, max_bytes);
        Self {
            retention,
            max_bytes,
            host_retention,
            inner: Arc::new(Mutex::new(inner)),
            writer,
            reader: reader.map(Mutex::new),
            id,
            head_watch: watch::Sender::new(head),
        }
    }

    pub fn id(&self) -> LogId {
        self.id
    }

    /// The newest event's `seq`, or 0 before the first.
    pub fn head(&self) -> u64 {
        self.inner().head
    }

    /// A receiver that wakes after every append.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.head_watch.subscribe()
    }

    /// Appends an event, stores it, and returns its `seq`. An event that can't be stored is still
    /// delivered from memory, and the failure is logged.
    pub async fn append(
        &self,
        time: Timestamp,
        project: Option<ProjectId>,
        event: WispEvent,
    ) -> u64 {
        match &self.writer {
            Some(writer) => writer
                .run(self.job(time, project, event))
                .await
                .unwrap_or_else(|| self.writer_unresponsive()),
            None => self.append_in_memory(time, project, event),
        }
    }

    /// [`EventLog::append`], for a caller with no tokio runtime context of its own, such as the
    /// project store's thread or the shared-context watcher's callback thread. Blocks the calling
    /// thread until the write finishes.
    pub fn append_blocking(
        &self,
        time: Timestamp,
        project: Option<ProjectId>,
        event: WispEvent,
    ) -> u64 {
        match &self.writer {
            Some(writer) => writer
                .run_blocking(self.job(time, project, event))
                .unwrap_or_else(|| self.writer_unresponsive()),
            None => self.append_in_memory(time, project, event),
        }
    }

    /// Practically unreachable: every job runs under `catch_unwind`, so the only way the writer
    /// thread fails to answer is if it could never be started, in which case `self.writer` is
    /// already `None`. Logs and reports a `seq` without touching any state, so the event is lost
    /// rather than risking a second, colliding assignment.
    fn writer_unresponsive(&self) -> u64 {
        error!("the event log's writer thread did not answer; the event was not delivered");
        self.head()
    }

    /// Builds the self-contained job the writer thread runs: assigning `event`'s `seq`, attempting
    /// to store it, and publishing it are all one synchronous call.
    fn job(
        &self,
        time: Timestamp,
        project: Option<ProjectId>,
        event: WispEvent,
    ) -> impl FnOnce(&Store) -> u64 + Send + 'static {
        let inner = Arc::clone(&self.inner);
        let head_watch = self.head_watch.clone();
        let (retention, max_bytes, host_retention) =
            (self.retention, self.max_bytes, self.host_retention);
        move |db: &Store| {
            let run_id = run_of(&event);
            let payload = serde_json::to_string(&event).unwrap_or_default();
            let bytes = payload.len();
            // The write itself must not hold this lock, or readers would wait on SQLite. No other
            // job can run between this and `publish` below: the writer thread runs one at a time.
            let seq = inner.lock().unwrap_or_else(PoisonError::into_inner).head + 1;
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
                // Only host and project events are pruned: a run's stay with its run row.
                if let Err(error) = db.prune_host_events(host_retention) {
                    error!(%error, "could not prune host and project events");
                }
            }
            let entry = Entry {
                seq,
                time,
                project,
                event,
                bytes,
            };
            inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .publish(&head_watch, retention, max_bytes, entry);
            seq
        }
    }

    /// [`EventLog::append`] for a log with no database: assigning the `seq` and publishing the
    /// entry happen under one lock, with no await point in between.
    fn append_in_memory(
        &self,
        time: Timestamp,
        project: Option<ProjectId>,
        event: WispEvent,
    ) -> u64 {
        let bytes = serde_json::to_string(&event).map_or(0, |json| json.len());
        let mut inner = self.inner();
        let seq = inner.head + 1;
        let entry = Entry {
            seq,
            time,
            project,
            event,
            bytes,
        };
        inner.publish(&self.head_watch, self.retention, self.max_bytes, entry);
        seq
    }

    /// `run`'s events after `after`, oldest first, from the database, or from memory for a log
    /// that has none: at most `limit` of them and about `max_bytes` of event JSON, but always at
    /// least one when any exists, so a page fits in a frame and paging always moves on. The
    /// flag says whether more follow. Reads through its own connection, so a slow page never
    /// blocks a concurrent append or vice versa.
    pub fn run_events(
        &self,
        run: RunId,
        after: u64,
        limit: usize,
        max_bytes: usize,
    ) -> Result<(Vec<Arc<Entry>>, bool), StoreError> {
        if let Some(reader) = &self.reader {
            let db = reader.lock().unwrap_or_else(PoisonError::into_inner);
            let (stored, more) = db.run_events(run.into(), after, limit, max_bytes)?;
            let entries = stored
                .iter()
                .map(|stored| Arc::new(entry(stored)))
                .collect();
            return Ok((entries, more));
        }
        let inner = self.inner();
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

    /// Removes `run`'s events from the in-memory replay window, for a deleted thread, so
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
        self.inner().start(after).map(|_| ())
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
        let index = inner.start(after)?;
        match inner
            .events
            .range(index..)
            .find(|event| event.project == project)
        {
            Some(event) => Ok((Some(Arc::clone(event)), event.seq)),
            None => Ok((None, inner.head)),
        }
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
    use std::sync::Arc;
    use std::time::Duration;

    use wisp_protocol::{AgentOutcome, AgentOutputItem, ProjectId, RunId, WispEvent};

    use super::{EventLog, Gone};

    fn append(log: &EventLog, project: Option<ProjectId>) -> u64 {
        log.append_blocking(jiff::Timestamp::now(), project, WispEvent::Unknown)
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
        log.append_blocking(jiff::Timestamp::now(), Some(project), finished(gone));
        log.append_blocking(jiff::Timestamp::now(), Some(project), finished(kept));
        log.append_blocking(jiff::Timestamp::now(), Some(project), finished(gone));

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
        first.append_blocking(jiff::Timestamp::now(), Some(project), finished(run));
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
        log.append_blocking(jiff::Timestamp::now(), None, finished(run));
        assert_eq!(log.head(), 1);
        log.append_blocking(jiff::Timestamp::now(), None, finished(run));
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

    /// Appends three `agent.output` events of the same size, 80 characters of text each.
    fn append_three_big(log: &EventLog, run: RunId) {
        for letter in ["a", "b", "c"] {
            let event = big_output(run, &letter.repeat(80));
            log.append_blocking(jiff::Timestamp::now(), None, event);
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
        append_three_big(&log, run);

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
        log.append_blocking(jiff::Timestamp::now(), None, finished(run)); // seq 2: run, never pruned
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
        append_three_big(&log, run);
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

    /// `run_events` reads through its own connection, so a page held open doesn't wait on, or
    /// make wait, a concurrent append.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_append_is_not_blocked_by_a_long_page_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wispd.sqlite3");
        let log = Arc::new(open(&path, 10));
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let reading = {
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                let reader = log
                    .reader
                    .as_ref()
                    .expect("a stored log has its own read connection");
                let _guard = reader.lock().unwrap();
                started_tx.send(()).unwrap();
                // Held "reading" until the test says otherwise, standing in for a slow page.
                release_rx.recv().unwrap();
            })
        };
        started_rx.recv().unwrap();

        let seq = tokio::time::timeout(
            Duration::from_secs(5),
            log.append(jiff::Timestamp::now(), None, WispEvent::Unknown),
        )
        .await
        .expect("an append waited on a concurrent page read");
        assert_eq!(seq, 1);

        release_tx.send(()).unwrap();
        reading.join().unwrap();
    }

    /// An `append` whose caller is dropped mid-wait must not leave the log's `seq` counter out of
    /// step with what SQLite has: occupy the writer thread, dispatch an append, abort its task
    /// before the job runs, release the writer, then append again. The second append must not
    /// reuse the first's `seq`, and the database must agree.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_aborted_appends_job_still_publishes_so_the_next_one_does_not_collide() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wispd.sqlite3");
        let log = Arc::new(open(&path, 10));

        // Occupies the writer thread on a plain OS thread (not async: `run_blocking` would panic
        // inside a runtime), so the append dispatched below is still queued, not yet started,
        // when its caller is aborted.
        let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let occupied = std::thread::spawn({
            let log = Arc::clone(&log);
            move || {
                let writer = log.writer.as_ref().expect("a stored log has a writer");
                writer.run_blocking(move |_db: &wisp_store::Store| {
                    started_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                });
            }
        });
        started_rx.recv().unwrap();

        let aborted = tokio::spawn({
            let log = Arc::clone(&log);
            async move {
                log.append(jiff::Timestamp::now(), None, WispEvent::Unknown)
                    .await
            }
        });
        // Long enough for the spawned task to run and dispatch its job (a channel send, not
        // waiting on anything); the writer thread stays occupied throughout, so this is not a
        // race against when the job itself runs, only against when it's queued.
        tokio::time::sleep(Duration::from_millis(50)).await;
        aborted.abort();
        let _ = aborted.await;

        // Let the writer thread move on to the aborted append's job, publishing it even though
        // nobody is waiting for it any more.
        release_tx.send(()).unwrap();
        occupied.join().unwrap();

        let seq = log
            .append(jiff::Timestamp::now(), None, WispEvent::Unknown)
            .await;
        assert_eq!(
            seq, 2,
            "the next append must not reuse the aborted append's seq"
        );
        assert_eq!(log.head(), 2);

        let raw = rusqlite::Connection::open(&path).unwrap();
        let seqs: Vec<i64> = raw
            .prepare("SELECT seq FROM events ORDER BY seq")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            seqs,
            [1, 2],
            "both events are stored under distinct seqs, agreeing with memory"
        );
    }
}
