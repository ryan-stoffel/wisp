//! SQLite-backed storage for wisp projects and key accounts.
//!
//! [`Store`] owns one SQLite connection and applies its own versioned
//! migrations on open. The caller chooses the database path; this crate
//! never constructs wisp's application data directory itself.
//!
//! A key account ([`Account`]) is metadata only: its API key lives in the macOS Keychain, never
//! in this database (#117).

mod accounts;
mod error;
mod migrations;
mod project;
mod timestamp;
mod usage;
mod worktree;

use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use rusqlite::{Connection, Error as SqliteError, ErrorCode};

pub use accounts::{Account, AccountFields};
pub use error::StoreError;
pub use project::{Project, ProjectFields};
pub use usage::{LimitSnapshot, SessionModelUsage, UsageDelta, UsageSummary};
pub use worktree::{Worktree, WorktreeFields};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const WAL_RETRY_INTERVAL: Duration = Duration::from_millis(20);

/// A connection to a wisp project database.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens the database at `path`, creating it and its parent directory
    /// on first run, and applying any pending migrations.
    ///
    /// # Errors
    ///
    /// Returns an error if the parent directory can't be created, if
    /// SQLite can't be opened or configured, or if the database was already
    /// migrated by a newer build of this crate
    /// ([`StoreError::UnsupportedSchemaVersion`]).
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }

        let mut conn = Connection::open(path)?;
        configure(&conn)?;
        migrations::run(&mut conn)?;

        Ok(Self { conn })
    }
}

/// Sets the pragmas every connection needs: a busy timeout so lock
/// contention waits instead of failing immediately, WAL journaling so
/// readers never block on a writer, and foreign key enforcement.
fn configure(conn: &Connection) -> Result<(), StoreError> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    set_wal_mode(conn)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    Ok(())
}

/// Sets `journal_mode=WAL`, retrying on `SQLITE_BUSY` for up to
/// `BUSY_TIMEOUT`.
///
/// This pragma needs its own retry loop: unlike ordinary reads and writes,
/// changing the journal mode does not go through SQLite's busy-handler
/// callback, so `Connection::busy_timeout` alone does not make it wait out
/// lock contention. Confirmed empirically — two connections racing to open
/// the same brand-new database made this pragma fail instantly with
/// `SQLITE_BUSY` well under a millisecond in, never waiting anywhere near
/// `BUSY_TIMEOUT` on its own.
fn set_wal_mode(conn: &Connection) -> Result<(), StoreError> {
    let deadline = Instant::now() + BUSY_TIMEOUT;
    loop {
        let attempt = conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0));
        match attempt {
            Ok(mode) => {
                let mode: String = mode;
                return if mode.eq_ignore_ascii_case("wal") {
                    Ok(())
                } else {
                    Err(StoreError::JournalMode(mode))
                };
            }
            Err(SqliteError::SqliteFailure(e, _))
                if e.code == ErrorCode::DatabaseBusy && Instant::now() < deadline =>
            {
                thread::sleep(WAL_RETRY_INTERVAL);
            }
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Store;

    #[test]
    fn open_enables_wal_busy_timeout_and_foreign_keys() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let store = Store::open(dir.path().join("wisp.sqlite3")).expect("open");

        let journal_mode: String = store
            .conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .expect("read journal_mode");
        assert_eq!(journal_mode, "wal");

        let foreign_keys: i64 = store
            .conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .expect("read foreign_keys");
        assert_eq!(foreign_keys, 1);

        let busy_timeout: i64 = store
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .expect("read busy_timeout");
        assert_eq!(busy_timeout, 5000, "busy_timeout should match BUSY_TIMEOUT");
    }
}
