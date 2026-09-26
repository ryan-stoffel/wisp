use jiff::Timestamp;
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use crate::Store;
use crate::error::StoreError;
use crate::timestamp;

/// One entry of 0007's event log, as stored (#156). The daemon owns what `kind` and `payload`
/// mean; this crate stores them as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// The daemon-wide sequence number, starting at 1.
    pub seq: u64,
    pub time: Timestamp,
    /// The project it belongs to, or `None` for a host-level event.
    pub project_id: Option<Uuid>,
    /// The agent run it belongs to, if any.
    pub run_id: Option<Uuid>,
    /// The event's `kind`, such as `agent.output`.
    pub kind: String,
    /// The event's JSON.
    pub payload: String,
}

struct RawEvent {
    seq: u64,
    time: String,
    project_id: Option<String>,
    run_id: Option<String>,
    kind: String,
    payload: String,
}

impl RawEvent {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            seq: row.get(0)?,
            time: row.get(1)?,
            project_id: row.get(2)?,
            run_id: row.get(3)?,
            kind: row.get(4)?,
            payload: row.get(5)?,
        })
    }

    fn into_event(self) -> Result<StoredEvent, StoreError> {
        let uuid = |text: Option<String>| -> Result<Option<Uuid>, StoreError> {
            text.map(|text| Uuid::parse_str(&text))
                .transpose()
                .map_err(Into::into)
        };
        Ok(StoredEvent {
            seq: self.seq,
            time: timestamp::parse(&self.time)?,
            project_id: uuid(self.project_id)?,
            run_id: uuid(self.run_id)?,
            kind: self.kind,
            payload: self.payload,
        })
    }
}

const COLUMNS: &str = "seq, time, project_id, run_id, kind, payload";

impl Store {
    /// The event log's id: the stored one, or `new_id`, stored now, for a log that has none yet.
    ///
    /// # Errors
    ///
    /// A database error, or an error if the stored id is corrupt.
    pub fn event_log_id(&self, new_id: Uuid) -> Result<Uuid, StoreError> {
        self.conn.execute(
            "INSERT INTO log_meta (id, log_id) VALUES (1, ?1) ON CONFLICT (id) DO NOTHING",
            params![new_id.to_string()],
        )?;
        let text: String =
            self.conn
                .query_row("SELECT log_id FROM log_meta WHERE id = 1", [], |row| {
                    row.get(0)
                })?;
        Ok(Uuid::parse_str(&text)?)
    }

    /// Makes this connection's commits durable against a crash of the process but not of the
    /// machine (`synchronous = NORMAL`, which SQLite recommends with WAL). The event log uses it
    /// for its own connection, which commits once per event.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn relax_sync(&self) -> Result<(), StoreError> {
        self.conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(())
    }

    /// Appends `event` to the log.
    ///
    /// # Errors
    ///
    /// A database error, including a constraint error if its `seq` is taken.
    pub fn append_event(&self, event: &StoredEvent) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO events (seq, time, project_id, run_id, kind, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.seq,
                timestamp::format(event.time),
                event.project_id.map(|id| id.to_string()),
                event.run_id.map(|id| id.to_string()),
                event.kind,
                event.payload,
            ],
        )?;
        Ok(())
    }

    /// The newest event's `seq`, or 0 for an empty log.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn event_head(&self) -> Result<u64, StoreError> {
        let head: Option<u64> = self
            .conn
            .query_row("SELECT MAX(seq) FROM events", [], |row| row.get(0))
            .optional()?
            .flatten();
        Ok(head.unwrap_or(0))
    }

    /// The newest `limit` events, oldest first.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or timestamp is corrupt.
    pub fn latest_events(&self, limit: usize) -> Result<Vec<StoredEvent>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM (
                SELECT {COLUMNS} FROM events ORDER BY seq DESC LIMIT ?1
             ) ORDER BY seq ASC"
        ))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![limit], RawEvent::from_row)?;
        rows.map(|row| row?.into_event()).collect()
    }

    /// Run `run_id`'s events after `after`, oldest first, at most `limit` of them.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or timestamp is corrupt.
    pub fn run_events(
        &self,
        run_id: Uuid,
        after: u64,
        limit: usize,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM events WHERE run_id = ?1 AND seq > ?2
             ORDER BY seq ASC LIMIT ?3"
        ))?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(
            params![run_id.to_string(), after, limit],
            RawEvent::from_row,
        )?;
        rows.map(|row| row?.into_event()).collect()
    }
}
