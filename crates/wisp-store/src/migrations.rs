use rusqlite::{Connection, TransactionBehavior, params};

use crate::error::StoreError;
use crate::timestamp;

struct Migration {
    version: i64,
    sql: &'static str,
}

/// Versioned migrations, applied in order. Add new tables here by appending
/// a migration; never edit one that has already shipped.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: "CREATE TABLE projects (
            id TEXT NOT NULL PRIMARY KEY,
            name TEXT NOT NULL,
            repo_path TEXT NOT NULL,
            host TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );",
    },
    // A wispd's store only holds projects on its own host, so the column
    // had nothing to say (decision record 0009).
    Migration {
        version: 2,
        sql: "ALTER TABLE projects DROP COLUMN host;",
    },
    // A key account's record: metadata only. The API key itself lives in the Keychain and never
    // reaches this database (#117); `masked_key` is the display form, such as `sk-ant-...abcd`.
    Migration {
        version: 3,
        sql: "CREATE TABLE accounts (
            id TEXT NOT NULL PRIMARY KEY,
            provider TEXT NOT NULL,
            label TEXT NOT NULL,
            masked_key TEXT NOT NULL,
            created_at TEXT NOT NULL
        );",
    },
    // Per-account usage (#120): append-only token/cost deltas, one replaced snapshot per account
    // limit window, and one replaced running total per session and model, so a resumed session
    // can pass its baseline (#113's `Resume::usage_totals`). `model` uses `''`, not `NULL`, as the
    // "no model reported" sentinel: SQLite treats two `NULL`s as distinct, which would stop
    // `session_usage_totals` from replacing an existing row for the unnamed model.
    Migration {
        version: 4,
        sql: "CREATE TABLE usage_deltas (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            run_id TEXT NOT NULL,
            account_id TEXT NOT NULL,
            model TEXT NOT NULL,
            input_tokens INTEGER NOT NULL,
            output_tokens INTEGER NOT NULL,
            cache_read_tokens INTEGER NOT NULL,
            cache_write_tokens INTEGER NOT NULL,
            cost_usd_micros INTEGER,
            at TEXT NOT NULL
        );
        CREATE INDEX usage_deltas_account_at ON usage_deltas (account_id, at);

        CREATE TABLE limit_snapshots (
            account_id TEXT NOT NULL,
            window TEXT NOT NULL,
            used_percent REAL,
            resets_at TEXT,
            captured_at TEXT NOT NULL,
            PRIMARY KEY (account_id, window)
        );

        CREATE TABLE session_usage_totals (
            session_id TEXT NOT NULL,
            model TEXT NOT NULL,
            input_tokens INTEGER NOT NULL,
            output_tokens INTEGER NOT NULL,
            cache_read_tokens INTEGER NOT NULL,
            cache_write_tokens INTEGER NOT NULL,
            cost_usd_micros INTEGER,
            PRIMARY KEY (session_id, model)
        );",
    },
];

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
