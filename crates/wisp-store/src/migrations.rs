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
    // A run's worktree (#154): one row per run, keyed by the run id, so startup garbage
    // collection can tell a worktree the store still knows about from an orphan left behind by
    // a crash. `base` is the concrete commit the worktree was created from, resolved once at
    // creation time so a later diff or commit is never against a base that moved.
    Migration {
        version: 5,
        sql: "CREATE TABLE worktrees (
            id TEXT NOT NULL PRIMARY KEY,
            repo_path TEXT NOT NULL,
            path TEXT NOT NULL,
            branch TEXT NOT NULL,
            base TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE INDEX worktrees_repo_path ON worktrees (repo_path);",
    },
    // Per-host role defaults for account routing (#119): the account a task's role falls back to
    // when it doesn't name one outright. `account_kind` is 'subscription' or 'key'; exactly one of
    // `backend` (a backend's name, such as 'claude') and `key_account_id` (a row in `accounts`,
    // though not enforced by a foreign key, since a key account can be removed after being set as
    // a default) is set, matching `account_kind`.
    Migration {
        version: 6,
        sql: "CREATE TABLE role_defaults (
            role TEXT NOT NULL PRIMARY KEY,
            account_kind TEXT NOT NULL,
            backend TEXT,
            key_account_id TEXT,
            updated_at TEXT NOT NULL
        );",
    },
    // Agent runs and the persisted event log (#156, decision 0014).
    //
    // - `worktrees.git_dir`: the linked worktree's private git directory, resolved once at
    //   creation (#166), so a restart never has to trust the worktree's own `.git` file again.
    // - `runs`: one row per `agent/start`, keyed by its client-generated run id. The params that
    //   make a retry idempotent (`project_id`, `prompt`, `requested_account`, `policy`) never
    //   change; the rest is the run's latest state. `requested_account` is the JSON of the
    //   account the request named, or NULL for the worker role's default.
    // - `log_meta` and `events`: 0007's event log, numbered by the daemon-wide `seq`, so the
    //   editor can replay after a reconnect or a restart. `payload` is the event's JSON;
    //   `run_id` is set for an agent run's events, for `agent/events`.
    Migration {
        version: 7,
        sql: "ALTER TABLE worktrees ADD COLUMN git_dir TEXT NOT NULL DEFAULT '';

        CREATE TABLE runs (
            id TEXT NOT NULL PRIMARY KEY,
            project_id TEXT NOT NULL,
            prompt TEXT NOT NULL,
            requested_account TEXT,
            policy TEXT NOT NULL,
            backend TEXT NOT NULL,
            account_id TEXT NOT NULL,
            status TEXT NOT NULL,
            session_id TEXT,
            error TEXT,
            commit_sha TEXT,
            files_changed INTEGER,
            insertions INTEGER,
            deletions INTEGER,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        CREATE INDEX runs_project ON runs (project_id, created_at);

        CREATE TABLE log_meta (
            id INTEGER NOT NULL PRIMARY KEY CHECK (id = 1),
            log_id TEXT NOT NULL
        );

        CREATE TABLE events (
            seq INTEGER NOT NULL PRIMARY KEY,
            time TEXT NOT NULL,
            project_id TEXT,
            run_id TEXT,
            kind TEXT NOT NULL,
            payload TEXT NOT NULL
        );
        CREATE INDEX events_run ON events (run_id, seq);",
    },
    // Accepting a run (#157): `agent/accept`'s client-generated id, for its idempotency, and what
    // the merge did to the project's repository (`merge_how` is `fastForward`, `merge`, or
    // `upToDate`). All NULL until the run is accepted.
    Migration {
        version: 8,
        sql: "ALTER TABLE runs ADD COLUMN accept_id TEXT;
        ALTER TABLE runs ADD COLUMN merge_commit TEXT;
        ALTER TABLE runs ADD COLUMN merge_into TEXT;
        ALTER TABLE runs ADD COLUMN merge_how TEXT;",
    },
    // Turn idempotency across a restart (#190): `Actor::turns` kept sent turns only in memory, so
    // a wispd restart lost `agent/send`'s idempotency (0014) for a run's `turnId`s, and a retried
    // `agent/send` could resume a session twice with the same message. No foreign key to `runs`,
    // matching this database's existing style (`role_defaults`, `events`).
    Migration {
        version: 9,
        sql: "CREATE TABLE turns (
            run_id TEXT NOT NULL,
            turn_id TEXT NOT NULL,
            text TEXT NOT NULL,
            created_at TEXT NOT NULL,
            PRIMARY KEY (run_id, turn_id)
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
