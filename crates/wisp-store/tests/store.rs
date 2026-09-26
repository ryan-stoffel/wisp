use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use uuid::Uuid;
use wisp_store::{
    AccountFields, ProjectFields, Store, StoreError, StoredEvent, UsageDelta, WorktreeFields,
};

fn temp_db_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("wisp.sqlite3");
    (dir, path)
}

fn sample_fields() -> ProjectFields {
    ProjectFields {
        name: "wisp".to_string(),
        repo_path: "/Users/ryan/dev/wisp".to_string(),
    }
}

fn sample_account_fields() -> AccountFields {
    AccountFields {
        provider: "anthropic".to_string(),
        label: "Personal".to_string(),
        masked_key: "sk-ant-...abcd".to_string(),
    }
}

fn sample_worktree_fields() -> WorktreeFields {
    WorktreeFields {
        repo_path: "/Users/ryan/dev/wisp".to_string(),
        path: "/Users/ryan/Library/Application Support/wisp/worktrees/wisp-abcd1234/\
               0199-run"
            .to_string(),
        branch: "wisp/abcd1234".to_string(),
        base: "b7e1f2a3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9".to_string(),
        git_dir: "/Users/ryan/dev/wisp/.git/worktrees/0199-run".to_string(),
    }
}

#[test]
fn opening_an_empty_database_creates_the_schema() {
    let (_dir, path) = temp_db_path();
    assert!(!path.exists());

    let store = Store::open(&path).expect("open should create the database");

    assert!(path.exists());
    assert_eq!(
        store.list_projects().expect("list should succeed"),
        Vec::new()
    );
}

#[test]
fn opening_a_database_creates_missing_parent_directories() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir
        .path()
        .join("nested")
        .join("deeper")
        .join("wisp.sqlite3");
    assert!(!path.parent().expect("path has a parent").exists());

    Store::open(&path).expect("open should create missing parent directories");

    assert!(path.exists());
}

#[test]
fn reopening_an_existing_database_keeps_its_data() {
    let (_dir, path) = temp_db_path();
    let id = Uuid::now_v7();

    {
        let mut store = Store::open(&path).expect("first open");
        store
            .create_project(id, &sample_fields())
            .expect("create should succeed");
    }

    let store = Store::open(&path).expect("second open should not re-run migrations destructively");
    let project = store
        .get_project(id)
        .expect("get should succeed")
        .expect("project should still exist after reopening");

    assert_eq!(project.name, sample_fields().name);
    assert_eq!(project.repo_path, sample_fields().repo_path);
}

#[test]
fn creating_with_the_same_id_and_fields_is_idempotent() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();

    let first = store
        .create_project(id, &sample_fields())
        .expect("first create should succeed");
    let second = store
        .create_project(id, &sample_fields())
        .expect("repeat create with identical fields should succeed");

    assert_eq!(first, second);
    assert_eq!(store.list_projects().expect("list").len(), 1);
}

#[test]
fn creating_with_the_same_id_and_different_fields_conflicts() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();
    let mut other = sample_fields();
    other.name = "different-name".to_string();

    store
        .create_project(id, &sample_fields())
        .expect("first create should succeed");
    let err = store
        .create_project(id, &other)
        .expect_err("repeat create with different fields should fail");

    match err {
        StoreError::IdConflict { id: conflicting } => assert_eq!(conflicting, id),
        other => panic!("expected IdConflict, got {other:?}"),
    }
    assert_eq!(
        store.list_projects().expect("list").len(),
        1,
        "a rejected conflicting create must not change the stored row"
    );
}

#[test]
fn update_replaces_fields_and_bumps_updated_at() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();
    let created = store
        .create_project(id, &sample_fields())
        .expect("create should succeed");

    let mut updated_fields = sample_fields();
    updated_fields.name = "renamed".to_string();
    let updated = store
        .update_project(id, &updated_fields)
        .expect("update of an existing project should succeed");

    assert_eq!(updated.name, "renamed");
    assert_eq!(updated.created_at, created.created_at);
    assert!(updated.updated_at >= created.updated_at);
}

#[test]
fn update_of_a_missing_project_fails_with_not_found() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();

    let err = store
        .update_project(id, &sample_fields())
        .expect_err("update of a missing project should fail");

    match err {
        StoreError::NotFound { id: missing } => assert_eq!(missing, id),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

#[test]
fn delete_removes_the_project_and_is_idempotent() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();
    store
        .create_project(id, &sample_fields())
        .expect("create should succeed");

    let deleted_first = store.delete_project(id).expect("delete should succeed");
    let deleted_second = store
        .delete_project(id)
        .expect("deleting twice should not error");

    assert!(
        deleted_first,
        "the first delete should report a row was removed"
    );
    assert!(
        !deleted_second,
        "the second delete should report nothing was removed"
    );
    assert_eq!(store.get_project(id).expect("get"), None);
}

/// Regression test for a migration-runner race: several connections opening
/// the same brand-new database at once must all succeed, and every
/// migration must be applied exactly once even though each connection reads
/// the "current version" before it holds any lock.
#[test]
fn concurrent_open_of_a_fresh_database_applies_migrations_exactly_once() {
    let (_dir, path) = temp_db_path();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            thread::spawn(move || Store::open(&path))
        })
        .collect();

    let stores: Vec<Store> = handles
        .into_iter()
        .map(|handle| handle.join().expect("opening thread should not panic"))
        .collect::<Result<_, StoreError>>()
        .expect("every concurrent open should succeed");

    for store in &stores {
        assert_eq!(store.list_projects().expect("list"), Vec::new());
    }

    let conn = Connection::open(&path).expect("open verification connection");
    let mut stmt = conn
        .prepare("SELECT version, COUNT(*) AS n FROM schema_version GROUP BY version HAVING n > 1")
        .expect("prepare duplicate check");
    let duplicate_versions: Vec<i64> = stmt
        .query_map([], |row| row.get(0))
        .expect("query duplicates")
        .collect::<Result<_, _>>()
        .expect("collect duplicates");

    assert!(
        duplicate_versions.is_empty(),
        "schema_version has duplicate rows for versions: {duplicate_versions:?}"
    );
}

/// WAL mode's key property: a reader on its own connection sees a
/// consistent snapshot and is never blocked by another connection's
/// still-open write transaction.
#[test]
fn concurrent_read_succeeds_while_another_connection_is_writing() {
    let (_dir, path) = temp_db_path();
    let reader = Store::open(&path).expect("open reader");

    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = Arc::clone(&barrier);
    let writer_path = path.clone();
    let writer = thread::spawn(move || {
        let mut conn = Connection::open(&writer_path).expect("open writer connection");
        conn.busy_timeout(Duration::from_secs(5))
            .expect("set busy timeout");
        let tx = conn.transaction().expect("begin write transaction");
        tx.execute(
            "INSERT INTO projects (id, name, repo_path, created_at, updated_at)
             VALUES ('01978c1e-70a0-7c3d-9b1a-000000000000', 'n', '/r', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("insert inside the open transaction");

        // Let the reader run its query while this transaction is still
        // uncommitted, then hold it open a little longer so the read can't
        // possibly land after a fast commit by accident.
        writer_barrier.wait();
        thread::sleep(Duration::from_millis(300));
        tx.commit().expect("commit");
    });

    barrier.wait();
    let seen_during_write = reader
        .list_projects()
        .expect("read during a concurrent write");
    writer.join().expect("writer thread should not panic");
    let seen_after_commit = reader
        .list_projects()
        .expect("read after the write committed");

    assert_eq!(
        seen_during_write.len(),
        0,
        "the reader's snapshot must not include the writer's uncommitted insert"
    );
    assert_eq!(
        seen_after_commit.len(),
        1,
        "a fresh read after commit must see the committed row"
    );
}

/// Regression test for the ordering bug fixed by storing a fixed-width
/// fraction: two projects created in the same second must come back from
/// `list_projects` in creation order. `time`'s RFC 3339 formatting trimmed
/// trailing zeros and dropped an all-zero fraction, so a later, "rounder"
/// timestamp could sort before an earlier one as TEXT.
#[test]
fn projects_created_in_the_same_second_list_oldest_first() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open");

    // Oldest to newest, same whole second, fixed-width fractions: the exact
    // shape `wisp-store` now writes.
    let oldest_first = [
        (Uuid::now_v7(), "2026-06-01T12:00:00.000000000Z"),
        (Uuid::now_v7(), "2026-06-01T12:00:00.100000000Z"),
        (Uuid::now_v7(), "2026-06-01T12:00:00.500000000Z"),
        (Uuid::now_v7(), "2026-06-01T12:00:00.500010000Z"),
    ];

    // Inserted out of order, so a passing test can only be explained by
    // `list_projects` sorting on value, not on insertion order.
    let conn = Connection::open(&path).expect("open raw connection");
    for (id, created_at) in [
        oldest_first[2],
        oldest_first[0],
        oldest_first[3],
        oldest_first[1],
    ] {
        conn.execute(
            &format!(
                "INSERT INTO projects (id, name, repo_path, created_at, updated_at)
                 VALUES ('{id}', 'n', '/r', '{created_at}', '{created_at}')"
            ),
            [],
        )
        .expect("insert row with a same-second fixed-width timestamp");
    }
    drop(conn);

    let listed_ids: Vec<Uuid> = store
        .list_projects()
        .expect("list")
        .into_iter()
        .map(|p| p.id)
        .collect();
    let expected_ids: Vec<Uuid> = oldest_first.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        listed_ids, expected_ids,
        "same-second projects must list oldest first"
    );
}

/// Regression test: a database written by the old `time`-based store (RFC
/// 3339 with a variable-width fraction, or none at all when it was zero)
/// must still open and read correctly. jiff's parser accepts any fraction
/// width.
#[test]
fn reads_timestamps_written_in_the_old_variable_width_format() {
    let (_dir, path) = temp_db_path();
    Store::open(&path).expect("open should create the schema");

    let id = Uuid::now_v7();
    let conn = Connection::open(&path).expect("open raw connection");
    conn.execute(
        &format!(
            "INSERT INTO projects (id, name, repo_path, created_at, updated_at)
             VALUES ('{id}', 'legacy', '/r', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00.5Z')"
        ),
        [],
    )
    .expect("insert a row shaped like time's old output");
    drop(conn);

    let store = Store::open(&path).expect("reopen an existing database");
    let project = store
        .get_project(id)
        .expect("get should parse the legacy timestamps")
        .expect("row should exist");

    assert_eq!(project.created_at, "2026-01-01T00:00:00Z".parse().unwrap());
    assert_eq!(
        project.updated_at,
        "2026-01-01T00:00:00.5Z".parse().unwrap()
    );
    assert_eq!(store.list_projects().expect("list").len(), 1);
}

/// Migration 2 drops the `host` column. A database written at schema
/// version 1, with a row in it, must open, keep the row, and lose the
/// column.
#[test]
fn a_version_1_database_migrates_and_keeps_its_projects() {
    let (_dir, path) = temp_db_path();
    let id = Uuid::now_v7();
    let conn = Connection::open(&path).expect("open raw connection");
    conn.execute_batch(&format!(
        "CREATE TABLE schema_version (
             version INTEGER NOT NULL PRIMARY KEY,
             applied_at TEXT NOT NULL
         );
         CREATE TABLE projects (
             id TEXT NOT NULL PRIMARY KEY,
             name TEXT NOT NULL,
             repo_path TEXT NOT NULL,
             host TEXT NOT NULL,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         INSERT INTO schema_version VALUES (1, '2026-09-24T12:00:00.000000000Z');
         INSERT INTO projects VALUES ('{id}', 'wisp', '/r', 'macbook',
             '2026-09-24T12:00:00.000000000Z', '2026-09-24T12:00:00.000000000Z');"
    ))
    .expect("write a version 1 database");
    drop(conn);

    let mut store = Store::open(&path).expect("open should migrate to version 2");
    let project = store
        .get_project(id)
        .expect("get")
        .expect("the row should survive the migration");
    assert_eq!(project.name, "wisp");
    assert_eq!(project.repo_path, "/r");
    let again = store
        .create_project(
            id,
            &ProjectFields {
                name: "wisp".to_string(),
                repo_path: "/r".to_string(),
            },
        )
        .expect("an idempotent create should match the migrated row");
    assert_eq!(again, project);

    let conn = Connection::open(&path).expect("open verification connection");
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('projects')")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(
        columns,
        ["id", "name", "repo_path", "created_at", "updated_at"]
    );
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .expect("read schema version");
    assert_eq!(
        version, 9,
        "migrations 3 through 7 (accounts, usage, worktrees, role defaults, runs and \
         events) and 9 (turns, #190) also apply"
    );
    let account_columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('accounts')")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(
        account_columns,
        ["id", "provider", "label", "masked_key", "created_at"]
    );
}

/// A database as `develop` (schema version 3, `accounts` but no usage tables) leaves it, opened by
/// a build that also knows migrations 4 (#120), 5 (worktrees, #154), and 6 (role defaults, #119).
/// Both the pre-existing `accounts` row and the new usage tables must be intact afterward.
#[test]
fn a_version_3_database_from_develop_migrates_to_usage_tables_and_keeps_its_account() {
    let (_dir, path) = temp_db_path();
    let account_id = Uuid::now_v7();
    let conn = Connection::open(&path).expect("open raw connection");
    conn.execute_batch(&format!(
        "CREATE TABLE schema_version (
             version INTEGER NOT NULL PRIMARY KEY,
             applied_at TEXT NOT NULL
         );
         CREATE TABLE projects (
             id TEXT NOT NULL PRIMARY KEY,
             name TEXT NOT NULL,
             repo_path TEXT NOT NULL,
             created_at TEXT NOT NULL,
             updated_at TEXT NOT NULL
         );
         CREATE TABLE accounts (
             id TEXT NOT NULL PRIMARY KEY,
             provider TEXT NOT NULL,
             label TEXT NOT NULL,
             masked_key TEXT NOT NULL,
             created_at TEXT NOT NULL
         );
         INSERT INTO schema_version VALUES
             (1, '2026-09-24T12:00:00.000000000Z'),
             (2, '2026-09-24T12:00:00.000000000Z'),
             (3, '2026-09-24T12:00:00.000000000Z');
         INSERT INTO accounts VALUES ('{account_id}', 'anthropic', 'Personal', 'sk-ant-...abcd',
             '2026-09-24T12:00:00.000000000Z');"
    ))
    .expect("write a version 3 database, as develop's #117 leaves it");
    drop(conn);

    let store = Store::open(&path).expect("open should migrate to the latest version");
    let account = store
        .get_account(account_id)
        .expect("get")
        .expect("the pre-existing account row should survive the migration");
    assert_eq!(account.masked_key, "sk-ant-...abcd");

    // The new usage tables work: a delta records and reads back.
    store
        .record_usage_delta(&UsageDelta {
            run_id: Uuid::now_v7(),
            account_id: account_id.to_string(),
            model: None,
            input_tokens: 5,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost_usd_micros: None,
            at: "2026-09-24T12:00:00Z".parse().unwrap(),
        })
        .expect("record a usage delta after migrating");
    let summary = store
        .usage_summary(
            &account_id.to_string(),
            "2026-01-01T00:00:00Z".parse().unwrap(),
            "2027-01-01T00:00:00Z".parse().unwrap(),
        )
        .expect("summary");
    assert_eq!(summary.input_tokens, 5);

    let conn = Connection::open(&path).expect("open verification connection");
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .expect("read schema version");
    assert_eq!(
        version, 9,
        "migrations 5 (worktrees, #154), 6 (role defaults, #119), 7 (runs and events, \
         #156), and 9 (turns, #190) also apply"
    );
}

#[test]
fn creating_an_account_with_the_same_id_and_fields_is_idempotent() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();

    let first = store
        .create_account(id, &sample_account_fields())
        .expect("first create should succeed");
    let second = store
        .create_account(id, &sample_account_fields())
        .expect("repeat create with identical fields should succeed");

    assert_eq!(first, second);
    assert_eq!(first.masked_key, "sk-ant-...abcd");
    assert_eq!(store.list_accounts().expect("list").len(), 1);
}

#[test]
fn creating_an_account_with_the_same_id_and_different_fields_conflicts() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();
    let mut other = sample_account_fields();
    other.label = "Work".to_string();

    store
        .create_account(id, &sample_account_fields())
        .expect("first create should succeed");
    let err = store
        .create_account(id, &other)
        .expect_err("repeat create with different fields should fail");

    match err {
        StoreError::IdConflict { id: conflicting } => assert_eq!(conflicting, id),
        other => panic!("expected IdConflict, got {other:?}"),
    }
    assert_eq!(
        store.list_accounts().expect("list").len(),
        1,
        "a rejected conflicting create must not change the stored row"
    );
}

#[test]
fn accounts_list_oldest_first_and_delete_removes_them_idempotently() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let first_id = Uuid::now_v7();
    store
        .create_account(first_id, &sample_account_fields())
        .expect("create first");
    let second_id = Uuid::now_v7();
    let mut second_fields = sample_account_fields();
    second_fields.provider = "openai".to_string();
    second_fields.label = "Work".to_string();
    second_fields.masked_key = "sk-proj-...wxyz".to_string();
    store
        .create_account(second_id, &second_fields)
        .expect("create second");

    let listed: Vec<Uuid> = store
        .list_accounts()
        .expect("list")
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert_eq!(listed, [first_id, second_id]);

    let deleted_first = store.delete_account(first_id).expect("delete");
    let deleted_second = store
        .delete_account(first_id)
        .expect("deleting twice should not error");
    assert!(deleted_first, "the first delete should report a removal");
    assert!(
        !deleted_second,
        "the second delete should report nothing removed"
    );
    assert_eq!(store.get_account(first_id).expect("get"), None);
    assert_eq!(store.list_accounts().expect("list").len(), 1);
}

/// A key account's row never holds the key itself: only its masked display form.
#[test]
fn account_rows_never_hold_more_than_the_masked_key() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();
    let secret = "sk-ant-api03-thisisaveryrealsecretabcd";
    store
        .create_account(
            id,
            &AccountFields {
                provider: "anthropic".to_string(),
                label: "Personal".to_string(),
                masked_key: "sk-ant-...abcd".to_string(),
            },
        )
        .expect("create");

    let conn = Connection::open(&path).expect("open raw connection");
    let columns: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('accounts')")
        .expect("prepare")
        .query_map([], |row| row.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("collect");
    assert_eq!(
        columns,
        ["id", "provider", "label", "masked_key", "created_at"],
        "no column exists for the real key"
    );
    let stored: String = conn
        .query_row(
            "SELECT masked_key FROM accounts WHERE id = ?1",
            [id.to_string()],
            |row| row.get(0),
        )
        .expect("read the stored masked_key");
    assert_ne!(stored, secret, "the real key must never be stored");
}

#[test]
fn creating_a_worktree_with_the_same_id_and_fields_is_idempotent() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();

    let first = store
        .create_worktree(id, &sample_worktree_fields())
        .expect("first create should succeed");
    let second = store
        .create_worktree(id, &sample_worktree_fields())
        .expect("repeat create with identical fields should succeed");

    assert_eq!(first, second);
    assert_eq!(first.branch, "wisp/abcd1234");
    assert_eq!(store.list_worktrees().expect("list").len(), 1);
}

#[test]
fn creating_a_worktree_with_the_same_id_and_different_fields_conflicts() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let id = Uuid::now_v7();
    let mut other = sample_worktree_fields();
    other.branch = "wisp/deadbeef".to_string();

    store
        .create_worktree(id, &sample_worktree_fields())
        .expect("first create should succeed");
    let err = store
        .create_worktree(id, &other)
        .expect_err("repeat create with different fields should fail");

    match err {
        StoreError::IdConflict { id: conflicting } => assert_eq!(conflicting, id),
        other => panic!("expected IdConflict, got {other:?}"),
    }
    assert_eq!(
        store.list_worktrees().expect("list").len(),
        1,
        "a rejected conflicting create must not change the stored row"
    );
}

#[test]
fn worktrees_list_oldest_first_and_delete_removes_them_idempotently() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let first_id = Uuid::now_v7();
    store
        .create_worktree(first_id, &sample_worktree_fields())
        .expect("create first");
    let second_id = Uuid::now_v7();
    let mut second_fields = sample_worktree_fields();
    second_fields.branch = "wisp/12345678".to_string();
    store
        .create_worktree(second_id, &second_fields)
        .expect("create second");

    let listed: Vec<Uuid> = store
        .list_worktrees()
        .expect("list")
        .into_iter()
        .map(|w| w.id)
        .collect();
    assert_eq!(listed, [first_id, second_id]);

    let deleted_first = store.delete_worktree(first_id).expect("delete");
    let deleted_second = store
        .delete_worktree(first_id)
        .expect("deleting twice should not error");
    assert!(deleted_first, "the first delete should report a removal");
    assert!(
        !deleted_second,
        "the second delete should report nothing removed"
    );
    assert_eq!(store.get_worktree(first_id).expect("get"), None);
    assert_eq!(store.list_worktrees().expect("list").len(), 1);
}

fn stored_event(seq: u64, run_id: Option<Uuid>) -> StoredEvent {
    StoredEvent {
        seq,
        time: "2026-09-25T12:00:00Z".parse().unwrap(),
        project_id: None,
        run_id,
        kind: if run_id.is_some() {
            "agent.output".to_string()
        } else {
            "context.changed".to_string()
        },
        payload: "{}".to_string(),
    }
}

#[test]
fn pruning_host_events_keeps_the_newest_and_leaves_run_events_alone() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open");
    let run = Uuid::now_v7();
    // Interleave host events (no run_id) with run events, so pruning has to pick them out by
    // `run_id IS NULL` rather than by a contiguous range of `seq`.
    for seq in 1..=6 {
        let run_id = if seq % 2 == 0 { Some(run) } else { None };
        store.append_event(&stored_event(seq, run_id)).unwrap();
    }

    let deleted = store.prune_host_events(1).expect("prune");
    assert_eq!(deleted, 2, "keeps only the newest of the 3 host events");

    let remaining = store.latest_events(usize::MAX, usize::MAX).expect("latest");
    let seqs: Vec<u64> = remaining.iter().map(|event| event.seq).collect();
    assert_eq!(
        seqs,
        [2, 4, 5, 6],
        "the run's events (2, 4, 6) all survive; only the newest host event (5) does"
    );

    let again = store.prune_host_events(1).expect("prune again");
    assert_eq!(again, 0, "already within the retention");
}
