//! SQLite-backed storage for wisp projects.
//!
//! [`Store`] owns one SQLite connection and applies its own versioned
//! migrations on open. The caller chooses the database path; this crate
//! never constructs wisp's application data directory itself.

mod error;
mod migrations;
mod project;
mod timestamp;

use std::fs;
use std::path::Path;
use std::time::Duration;

use rusqlite::Connection;

pub use error::StoreError;
pub use project::{Project, ProjectFields};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

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

/// Sets the pragmas every connection needs: WAL journaling so readers never
/// block on a writer, a busy timeout so a brief lock contention waits
/// instead of failing immediately, and foreign key enforcement.
fn configure(conn: &Connection) -> Result<(), StoreError> {
    let mode: String =
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::JournalMode(mode));
    }

    conn.busy_timeout(BUSY_TIMEOUT)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    Ok(())
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
