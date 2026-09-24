use rusqlite::{Connection, TransactionBehavior, params};

use crate::error::StoreError;
use crate::timestamp;

struct Migration {
    version: i64,
    sql: &'static str,
}

/// Versioned migrations, applied in order. Add new tables here by appending
/// a migration; never edit one that has already shipped.
const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    sql: "CREATE TABLE projects (
        id TEXT NOT NULL PRIMARY KEY,
        name TEXT NOT NULL,
        repo_path TEXT NOT NULL,
        host TEXT NOT NULL,
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );",
}];

/// Bootstraps the `schema_version` table and applies any migration whose
/// version is newer than what's recorded.
pub(crate) fn run(conn: &mut Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL PRIMARY KEY,
            applied_at TEXT NOT NULL
        );",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )?;

    let supported = MIGRATIONS.last().map_or(0, |m| m.version);
    if current > supported {
        return Err(StoreError::UnsupportedSchemaVersion {
            found: current,
            supported,
        });
    }

    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        // `current` was read before this transaction acquired the write
        // lock, so another connection may have applied this exact
        // migration in the meantime (two `Store::open` calls racing to
        // create the same brand-new database). Re-check under the lock,
        // which now sees that connection's commit rather than our stale
        // pre-lock snapshot, and skip re-applying it if so.
        let already_applied: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM schema_version WHERE version = ?1)",
            params![migration.version],
            |row| row.get(0),
        )?;

        if !already_applied {
            tx.execute_batch(migration.sql)?;
            tx.execute(
                "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
                params![migration.version, timestamp::now()],
            )?;
        }

        tx.commit()?;
    }

    Ok(())
}
