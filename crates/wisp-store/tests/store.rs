use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use rusqlite::types::FromSql;
use uuid::Uuid;
use wisp_store::{
    AccountFields, LimitSnapshot, ProjectFields, RepoFields, Run, RunAccept, RunFields, RunState,
    SessionModelUsage, Store, StoreError, StoredEvent, UsageDelta, WorktreeFields,
};

fn temp_db_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("wisp.sqlite3");
    (dir, path)
}

fn open() -> (tempfile::TempDir, Store) {
    let (dir, path) = temp_db_path();
    (dir, Store::open(path).expect("open"))
}

fn project_fields() -> ProjectFields {
    ProjectFields {
        name: "wisp".to_string(),
        repo_path: "/Users/ryan/dev/wisp".to_string(),
    }
}

fn account_fields() -> AccountFields {
    AccountFields {
        provider: "anthropic".to_string(),
        label: "Personal".to_string(),
        masked_key: "sk-ant-...abcd".to_string(),
    }
}

fn run_fields(project_id: Uuid) -> RunFields {
    RunFields {
        project_id,
        prompt: "Add a README".to_owned(),
        requested_account: Some(r#"{"kind":"subscription","backend":"claude"}"#.to_owned()),
        policy: "workspaceWrite".to_owned(),
        backend: "claude".to_owned(),
    }
}

fn starting() -> RunState {
    RunState {
        status: "starting".to_owned(),
        account_id: "claude".to_owned(),
        ..RunState::default()
    }
}

fn worktree_fields(id: Uuid) -> WorktreeFields {
    WorktreeFields {
        repo_path: "/Users/me/src/wisp".to_owned(),
        path: format!("/data/worktrees/wisp-1234/{id}"),
        branch: format!("wisp/{}", &id.simple().to_string()[..8]),
        base: "b7e1f2a3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9".to_owned(),
        git_dir: format!("/Users/me/src/wisp/.git/worktrees/{id}"),
    }
}

fn event(seq: u64, run_id: Option<Uuid>) -> StoredEvent {
    StoredEvent {
        seq,
        time: "2026-09-25T12:00:00.25Z".parse().unwrap(),
        project_id: Some(Uuid::now_v7()),
        run_id,
        kind: "agent.output".to_owned(),
        payload: format!(r#"{{"kind":"agent.output","n":{seq}}}"#),
    }
}

fn column<T: FromSql>(conn: &Connection, sql: &str) -> Vec<T> {
    conn.prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn schema_version(path: &Path) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
            row.get(0)
        })
        .unwrap()
}

#[test]
fn opening_creates_missing_parent_directories_and_the_schema() {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir
        .path()
        .join("nested")
        .join("deeper")
        .join("wisp.sqlite3");

    let store = Store::open(&path).expect("open should create the database and its parents");

    assert!(path.exists());
    assert_eq!(store.list_projects().unwrap(), Vec::new());
}

#[test]
fn reopening_an_existing_database_keeps_its_data() {
    let (_dir, path) = temp_db_path();
    let id = Uuid::now_v7();
    let created = Store::open(&path)
        .unwrap()
        .create_project(id, &project_fields())
        .unwrap();

    let store = Store::open(&path).expect("second open should not re-run migrations destructively");
    assert_eq!(store.get_project(id).unwrap(), Some(created));
}

#[test]
fn creating_a_project_is_idempotent_on_its_id_and_fields() {
    let (_dir, mut store) = open();
    let id = Uuid::now_v7();

    let first = store.create_project(id, &project_fields()).unwrap();
    let second = store.create_project(id, &project_fields()).unwrap();
    assert_eq!(first, second);

    let other = ProjectFields {
        name: "different-name".to_string(),
        ..project_fields()
    };
    assert!(matches!(
        store.create_project(id, &other),
        Err(StoreError::IdConflict { id: conflicting }) if conflicting == id
    ));
    assert_eq!(store.list_projects().unwrap(), [first]);
}

/// Several connections opening the same brand-new database at once must all succeed, and every
/// migration must be applied exactly once even though each connection reads the "current
/// version" before it holds any lock.
#[test]
fn concurrent_open_of_a_fresh_database_applies_migrations_exactly_once() {
    let (_dir, path) = temp_db_path();

    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            thread::spawn(move || Store::open(&path))
        })
        .collect();
    for handle in handles {
        handle
            .join()
            .expect("opening thread should not panic")
            .expect("every concurrent open should succeed");
    }

    let duplicate_versions: Vec<i64> = column(
        &Connection::open(&path).unwrap(),
        "SELECT version, COUNT(*) AS n FROM schema_version GROUP BY version HAVING n > 1",
    );
    assert!(
        duplicate_versions.is_empty(),
        "schema_version has duplicate rows for versions: {duplicate_versions:?}"
    );
}

/// WAL mode's key property: a reader on its own connection sees a consistent snapshot and is
/// never blocked by another connection's still-open write transaction.
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

        // Let the reader run its query while this transaction is still uncommitted, then hold it
        // open a little longer so the read can't possibly land after a fast commit by accident.
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

/// A database written by the old `time`-based store (RFC 3339 with a variable-width fraction, or
/// none at all when it was zero) must still read correctly.
#[test]
fn reads_timestamps_written_in_the_old_variable_width_format() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open should create the schema");

    let id = Uuid::now_v7();
    Connection::open(&path)
        .unwrap()
        .execute(
            &format!(
                "INSERT INTO projects (id, name, repo_path, created_at, updated_at)
                 VALUES ('{id}', 'legacy', '/r', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00.5Z')"
            ),
            [],
        )
        .expect("insert a row shaped like time's old output");

    let project = store
        .get_project(id)
        .expect("get should parse the legacy timestamps")
        .expect("row should exist");
    assert_eq!(project.created_at, "2026-01-01T00:00:00Z".parse().unwrap());
    assert_eq!(
        project.updated_at,
        "2026-01-01T00:00:00.5Z".parse().unwrap()
    );
}

/// Migration 2 drops the `host` column. A database written at schema version 1, with a row in
/// it, must open, keep the row, lose the column, and gain every later migration.
#[test]
fn a_version_1_database_migrates_and_keeps_its_projects() {
    let (_dir, path) = temp_db_path();
    let id = Uuid::now_v7();
    Connection::open(&path)
        .unwrap()
        .execute_batch(&format!(
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

    let mut store = Store::open(&path).expect("open should migrate");
    let project = store
        .get_project(id)
        .unwrap()
        .expect("the row should survive the migration");
    let fields = ProjectFields {
        name: "wisp".to_string(),
        repo_path: "/r".to_string(),
    };
    assert_eq!(
        store.create_project(id, &fields).unwrap(),
        project,
        "an idempotent create should match the migrated row"
    );

    let conn = Connection::open(&path).unwrap();
    assert_eq!(
        column::<String>(&conn, "SELECT name FROM pragma_table_info('projects')"),
        ["id", "name", "repo_path", "created_at", "updated_at"]
    );
    assert_eq!(
        column::<String>(&conn, "SELECT name FROM pragma_table_info('accounts')"),
        ["id", "provider", "label", "masked_key", "created_at"],
        "no column holds a real key"
    );
    assert_eq!(schema_version(&path), 10);
}

#[test]
fn accounts_are_idempotent_on_id_list_oldest_first_and_delete_once() {
    let (_dir, mut store) = open();
    let first_id = Uuid::now_v7();
    let first = store.create_account(first_id, &account_fields()).unwrap();
    assert_eq!(
        store.create_account(first_id, &account_fields()).unwrap(),
        first
    );
    let relabeled = AccountFields {
        label: "Work".to_string(),
        ..account_fields()
    };
    assert!(matches!(
        store.create_account(first_id, &relabeled),
        Err(StoreError::IdConflict { id }) if id == first_id
    ));

    let second_id = Uuid::now_v7();
    let second_fields = AccountFields {
        provider: "openai".to_string(),
        label: "Work".to_string(),
        masked_key: "sk-proj-...wxyz".to_string(),
    };
    store.create_account(second_id, &second_fields).unwrap();
    let listed: Vec<Uuid> = store
        .list_accounts()
        .unwrap()
        .into_iter()
        .map(|a| a.id)
        .collect();
    assert_eq!(listed, [first_id, second_id]);

    assert!(store.delete_account(first_id).unwrap());
    assert!(!store.delete_account(first_id).unwrap());
    assert_eq!(store.get_account(first_id).unwrap(), None);
    assert_eq!(store.list_accounts().unwrap().len(), 1);
}

#[test]
fn pruning_host_events_keeps_the_newest_and_leaves_run_events_alone() {
    let (_dir, store) = open();
    let run = Uuid::now_v7();
    // Interleave host events (no run_id) with run events, so pruning has to pick them out by
    // `run_id IS NULL` rather than by a contiguous range of `seq`.
    for seq in 1..=6 {
        let run_id = if seq % 2 == 0 { Some(run) } else { None };
        store.append_event(&event(seq, run_id)).unwrap();
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

#[test]
fn a_run_is_created_once_read_back_and_updated() {
    let (_dir, store) = open();
    let (project, id) = (Uuid::now_v7(), Uuid::now_v7());
    let created = store
        .create_run(id, &run_fields(project), &starting())
        .unwrap();
    assert_eq!(created.id, id);
    assert_eq!(created.fields, run_fields(project));
    assert_eq!(created.state, starting());
    assert_eq!(store.get_run(id).unwrap(), Some(created.clone()));

    assert!(matches!(
        store.create_run(id, &run_fields(project), &starting()),
        Err(StoreError::IdConflict { id: conflict }) if conflict == id
    ));

    let finished = RunState {
        status: "completed".to_owned(),
        account_id: "0199-key".to_owned(),
        session_id: Some("session-1".to_owned()),
        error: None,
        commit_sha: Some("abc123".to_owned()),
        files_changed: Some(2),
        insertions: Some(10),
        deletions: Some(1),
        accept: None,
    };
    let updated = store.update_run(id, &finished).unwrap();
    assert_eq!(updated.state, finished);
    assert_eq!(
        updated.fields,
        run_fields(project),
        "the request never changes"
    );
    assert!(updated.updated_at >= created.updated_at);
    assert_eq!(updated.created_at, created.created_at);

    let missing = Uuid::now_v7();
    assert!(matches!(
        store.update_run(missing, &finished),
        Err(StoreError::NotFound { id }) if id == missing
    ));
    assert_eq!(store.get_run(missing).unwrap(), None);
}

#[test]
fn accepting_a_run_records_the_merge_and_drops_its_worktree_together() {
    let (_dir, mut store) = open();
    let (project, id) = (Uuid::now_v7(), Uuid::now_v7());
    store
        .create_run_with_worktree(id, &run_fields(project), &starting(), &worktree_fields(id))
        .unwrap();
    let accepted = RunState {
        status: "accepted".to_owned(),
        commit_sha: Some("def456".to_owned()),
        accept: Some(RunAccept {
            id: Uuid::now_v7(),
            commit: "def456".to_owned(),
            into: "main".to_owned(),
            how: "fastForward".to_owned(),
        }),
        ..starting()
    };
    let run = store.accept_run(id, &accepted).unwrap();
    assert_eq!(run.state, accepted);
    assert_eq!(store.get_run(id).unwrap().unwrap().state, accepted);
    assert_eq!(store.get_worktree(id).unwrap(), None);

    let missing = Uuid::now_v7();
    assert!(matches!(
        store.accept_run(missing, &accepted),
        Err(StoreError::NotFound { id }) if id == missing
    ));
}

#[test]
fn runs_list_oldest_first_and_by_project() {
    let (_dir, store) = open();
    let (one, two) = (Uuid::now_v7(), Uuid::now_v7());
    let first = store
        .create_run(Uuid::now_v7(), &run_fields(one), &starting())
        .unwrap();
    let second = store
        .create_run(Uuid::now_v7(), &run_fields(two), &starting())
        .unwrap();
    let third = store
        .create_run(Uuid::now_v7(), &run_fields(one), &starting())
        .unwrap();
    let ids = |runs: Vec<Run>| runs.into_iter().map(|run| run.id).collect::<Vec<_>>();
    assert_eq!(
        ids(store.list_runs(None).unwrap()),
        [first.id, second.id, third.id]
    );
    assert_eq!(
        ids(store.list_runs(Some(one)).unwrap()),
        [first.id, third.id]
    );
    assert!(store.list_runs(Some(Uuid::now_v7())).unwrap().is_empty());
}

#[test]
fn the_log_id_is_stored_once_until_reset() {
    let (dir, store) = open();
    let first = Uuid::now_v7();
    assert_eq!(store.event_log_id(first).unwrap(), first);
    assert_eq!(store.event_log_id(Uuid::now_v7()).unwrap(), first);
    drop(store);
    let reopened = Store::open(dir.path().join("wisp.sqlite3")).unwrap();
    assert_eq!(reopened.event_log_id(Uuid::now_v7()).unwrap(), first);

    reopened.reset_event_log_id().unwrap();
    let second = Uuid::now_v7();
    assert_eq!(reopened.event_log_id(second).unwrap(), second);
}

#[test]
fn events_append_and_read_back_by_head_tail_and_run() {
    let (_dir, store) = open();
    store.relax_sync().unwrap();
    assert_eq!(store.event_head().unwrap(), 0);
    assert!(store.latest_events(10, usize::MAX).unwrap().is_empty());

    let run = Uuid::now_v7();
    let events: Vec<StoredEvent> = (1..=5)
        .map(|seq| event(seq, (seq % 2 == 1).then_some(run)))
        .collect();
    for event in &events {
        store.append_event(event).unwrap();
    }
    assert!(
        store.append_event(&events[0]).is_err(),
        "a seq is used once"
    );
    assert_eq!(store.event_head().unwrap(), 5);
    assert_eq!(store.latest_events(2, usize::MAX).unwrap(), events[3..]);
    assert_eq!(store.latest_events(100, usize::MAX).unwrap(), events);

    // The byte bound applies the same way: always at least one, and it stops before a row that
    // would put it over budget rather than after.
    let one = events[4].payload.len();
    assert_eq!(
        store.latest_events(100, one).unwrap(),
        events[4..],
        "the byte bound alone keeps just the newest event"
    );
    assert_eq!(
        store
            .latest_events(100, one + events[3].payload.len())
            .unwrap(),
        events[3..],
        "raising it by exactly the next event's size admits that one too"
    );

    let page = |after, limit, bytes| {
        let (events, more) = store.run_events(run, after, limit, bytes).unwrap();
        (events.iter().map(|e| e.seq).collect::<Vec<_>>(), more)
    };
    assert_eq!(page(0, 100, usize::MAX), (vec![1, 3, 5], false));
    assert_eq!(page(1, 1, usize::MAX), (vec![3], true));
    assert_eq!(page(5, 100, usize::MAX), (vec![], false));
    assert_eq!(
        store.run_events(run, 0, 1, usize::MAX).unwrap().0[0],
        events[0]
    );
}

#[test]
fn a_page_of_run_events_stops_at_its_byte_budget_but_never_comes_back_empty() {
    let (_dir, store) = open();
    let run = Uuid::now_v7();
    for seq in 1..=10 {
        let mut big = event(seq, Some(run));
        big.payload = "x".repeat(1000);
        store.append_event(&big).unwrap();
    }
    let (first, more) = store.run_events(run, 0, 500, 2500).unwrap();
    assert_eq!(first.len(), 2, "a third would pass 2500 bytes");
    assert!(more);
    let (tiny, more) = store.run_events(run, 0, 500, 10).unwrap();
    assert_eq!(
        tiny.len(),
        1,
        "one event even when it alone is over the budget"
    );
    assert!(more);

    let mut after = 0;
    let mut seen = Vec::new();
    loop {
        let (events, more) = store.run_events(run, after, 500, 2500).unwrap();
        after = events.last().unwrap().seq;
        seen.extend(events.iter().map(|e| e.seq));
        if !more {
            break;
        }
    }
    assert_eq!(seen, (1..=10).collect::<Vec<_>>());
}

#[test]
fn a_run_and_its_worktree_are_created_together_or_not_at_all() {
    let (_dir, mut store) = open();
    let id = Uuid::now_v7();
    let (run, worktree) = store
        .create_run_with_worktree(
            id,
            &run_fields(Uuid::now_v7()),
            &starting(),
            &worktree_fields(id),
        )
        .unwrap();
    assert_eq!(store.get_run(id).unwrap(), Some(run));
    assert_eq!(store.get_worktree(id).unwrap(), Some(worktree));

    // A run row already there leaves no new worktree row behind.
    let run_only = Uuid::now_v7();
    store
        .create_run(run_only, &run_fields(Uuid::now_v7()), &starting())
        .unwrap();
    assert!(
        store
            .create_run_with_worktree(
                run_only,
                &run_fields(Uuid::now_v7()),
                &starting(),
                &worktree_fields(run_only)
            )
            .is_err()
    );
    assert_eq!(store.get_worktree(run_only).unwrap(), None);
}

#[test]
fn a_version_6_database_gains_runs_events_and_worktree_git_dirs() {
    let (_dir, path) = temp_db_path();
    let id = Uuid::now_v7();
    Store::open(&path)
        .unwrap()
        .create_run_with_worktree(
            id,
            &run_fields(Uuid::now_v7()),
            &starting(),
            &worktree_fields(id),
        )
        .unwrap();
    // Roll the database back to schema 6: no runs, events, turns, threads, or repos tables, and
    // no git_dir column. Dropping `runs` also drops migration 8's columns on it.
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "DROP TABLE runs; DROP TABLE log_meta; DROP TABLE events; DROP TABLE turns;
             DROP TABLE threads; DROP TABLE repos;
             ALTER TABLE worktrees DROP COLUMN git_dir;
             DELETE FROM schema_version WHERE version >= 7;",
        )
        .unwrap();
    let store = Store::open(&path).unwrap();
    let worktree = store.get_worktree(id).unwrap().unwrap();
    assert_eq!(worktree.git_dir, "", "an older row has no pinned git dir");
    let run = store
        .create_run(Uuid::now_v7(), &run_fields(Uuid::now_v7()), &starting())
        .unwrap();
    assert_eq!(store.list_runs(None).unwrap(), [run]);
    store.append_event(&event(1, None)).unwrap();
    assert_eq!(store.event_head().unwrap(), 1);
}

/// Two branches each added a migration: a database that has a newer version but is missing an
/// older one still gets the older one.
#[test]
fn a_missing_migration_below_the_newest_still_applies() {
    let (_dir, path) = temp_db_path();
    drop(Store::open(&path).unwrap());
    let versions = "SELECT version FROM schema_version ORDER BY version";
    let conn = Connection::open(&path).unwrap();
    let all: Vec<i64> = column(&conn, versions);
    conn.execute_batch("DELETE FROM schema_version WHERE version = 6; DROP TABLE role_defaults;")
        .unwrap();
    drop(conn);

    drop(Store::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    assert_eq!(column::<i64>(&conn, versions), all);
    let restored: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'role_defaults'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(restored, 1);
}

fn repo_fields(path: &str) -> RepoFields {
    RepoFields {
        name: path.rsplit('/').next().unwrap().to_owned(),
        path: path.to_owned(),
        scratch: false,
    }
}

#[test]
fn a_path_has_one_repo_entry_whatever_id_asks_for_it() {
    let (_dir, mut store) = open();
    let first = Uuid::now_v7();
    let repo = store
        .add_repo(first, &repo_fields("/Users/me/src/wisp"))
        .unwrap();
    assert_eq!(repo.id, first);
    assert_eq!(repo.fields.name, "wisp");
    assert!(!repo.fields.scratch);

    let again = store
        .add_repo(Uuid::now_v7(), &repo_fields("/Users/me/src/wisp"))
        .unwrap();
    assert_eq!(
        again, repo,
        "a second id for the same path gets the first entry"
    );
    let retried = store
        .add_repo(first, &repo_fields("/Users/me/src/wisp"))
        .unwrap();
    assert_eq!(retried, repo);

    let conflict = store
        .add_repo(first, &repo_fields("/Users/me/src/other"))
        .unwrap_err();
    assert!(matches!(conflict, StoreError::IdConflict { id } if id == first));

    let other = store
        .add_repo(Uuid::now_v7(), &repo_fields("/Users/me/src/other"))
        .unwrap();
    assert_eq!(store.list_repos().unwrap(), vec![repo.clone(), other]);
    assert_eq!(store.get_repo(first).unwrap(), Some(repo));
    assert_eq!(store.scratch_repo().unwrap(), None);

    let scratch = store
        .add_repo(
            Uuid::now_v7(),
            &RepoFields {
                name: "No Repo".to_owned(),
                path: "/data/scratch".to_owned(),
                scratch: true,
            },
        )
        .unwrap();
    assert_eq!(store.scratch_repo().unwrap(), Some(scratch));
}

#[test]
fn a_thread_is_recorded_with_its_run_and_worktree_and_archives() {
    let (_dir, mut store) = open();
    let repo = store
        .add_repo(Uuid::now_v7(), &repo_fields("/Users/me/src/wisp"))
        .unwrap();
    let id = Uuid::now_v7();
    let create = |store: &mut Store| {
        store.create_thread_run(
            id,
            repo.id,
            &run_fields(repo.id),
            &starting(),
            &worktree_fields(id),
        )
    };
    let (thread, run, worktree) = create(&mut store).unwrap();
    assert_eq!(thread.id, id);
    assert_eq!(thread.repo_id, repo.id);
    assert!(!thread.archived);
    assert_eq!(run.fields.project_id, repo.id);
    assert_eq!(worktree.id, id);
    assert_eq!(store.list_runs(Some(repo.id)).unwrap(), vec![run]);

    assert!(matches!(
        create(&mut store).unwrap_err(),
        StoreError::IdConflict { .. }
    ));
    assert_eq!(store.list_threads().unwrap(), vec![thread.clone()]);

    let archived = store.set_thread_archived(id, true).unwrap();
    assert!(archived.archived);
    assert_eq!(store.get_thread(id).unwrap(), Some(archived));
    assert!(!store.set_thread_archived(id, false).unwrap().archived);
    let missing = Uuid::now_v7();
    assert!(matches!(
        store.set_thread_archived(missing, true).unwrap_err(),
        StoreError::NotFound { id } if id == missing
    ));
}

#[test]
fn deleting_a_thread_removes_its_run_worktree_events_and_turns_only() {
    let (_dir, mut store) = open();
    let repo = store
        .add_repo(Uuid::now_v7(), &repo_fields("/Users/me/src/wisp"))
        .unwrap();
    let [kept, gone] = [Uuid::now_v7(), Uuid::now_v7()];
    for id in [kept, gone] {
        store
            .create_thread_run(
                id,
                repo.id,
                &run_fields(repo.id),
                &starting(),
                &worktree_fields(id),
            )
            .unwrap();
        store.record_turn(id, Uuid::now_v7(), "carry on").unwrap();
    }
    for (seq, run) in [(1, kept), (2, gone), (3, gone)] {
        store.append_event(&event(seq, Some(run))).unwrap();
    }

    assert!(store.delete_thread(gone).unwrap());
    assert!(
        !store.delete_thread(gone).unwrap(),
        "deleting again does nothing"
    );
    assert_eq!(store.get_thread(gone).unwrap(), None);
    assert_eq!(store.get_run(gone).unwrap(), None);
    assert_eq!(store.get_worktree(gone).unwrap(), None);
    assert!(store.run_events(gone, 0, 10, 1 << 20).unwrap().0.is_empty());
    assert_eq!(store.run_turns(gone).unwrap(), []);

    assert!(store.get_thread(kept).unwrap().is_some());
    assert!(store.get_run(kept).unwrap().is_some());
    assert_eq!(store.run_events(kept, 0, 10, 1 << 20).unwrap().0.len(), 1);
    assert_eq!(store.run_turns(kept).unwrap().len(), 1);
}

fn delta(account_id: &str, at: &str, input: u64) -> UsageDelta {
    UsageDelta {
        run_id: Uuid::now_v7(),
        account_id: account_id.to_owned(),
        model: Some("claude-opus".to_owned()),
        input_tokens: input,
        output_tokens: input,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd_micros: Some(input * 10),
        at: at.parse().expect("parse timestamp"),
    }
}

/// The day boundary is `[start, end)`: a delta exactly at the end of a day belongs to the next
/// day, and one exactly at the start of a day belongs to it.
#[test]
fn deltas_sum_over_a_half_open_range() {
    let (_dir, store) = open();
    for (at, input) in [
        ("2026-09-23T23:59:59Z", 1),
        ("2026-09-24T00:00:00Z", 2),
        ("2026-09-24T23:59:59.999999999Z", 4),
        ("2026-09-25T00:00:00Z", 8),
    ] {
        store.record_usage_delta(&delta("acct", at, input)).unwrap();
    }
    let summary = |since: &str| {
        store
            .usage_summary(
                "acct",
                since.parse().unwrap(),
                "2026-09-25T00:00:00Z".parse().unwrap(),
            )
            .unwrap()
    };

    let today = summary("2026-09-24T00:00:00Z");
    assert_eq!(today.input_tokens, 2 + 4, "half-open range: [start, end)");
    assert_eq!(today.output_tokens, 2 + 4);
    assert_eq!(today.cost_usd_micros, Some(60));
    assert_eq!(
        summary("2026-09-21T00:00:00Z").input_tokens,
        1 + 2 + 4,
        "the week also excludes the 25th"
    );
}

#[test]
fn cost_is_not_reported_when_no_delta_in_range_reported_one() {
    let (_dir, store) = open();
    let mut no_cost = delta("codex", "2026-09-24T10:00:00Z", 100);
    no_cost.cost_usd_micros = None;
    store.record_usage_delta(&no_cost).unwrap();

    let summary = |account_id| {
        store
            .usage_summary(
                account_id,
                "2026-09-24T00:00:00Z".parse().unwrap(),
                "2026-09-25T00:00:00Z".parse().unwrap(),
            )
            .unwrap()
    };
    let codex = summary("codex");
    assert_eq!(codex.input_tokens, 100, "tokens are still counted");
    assert_eq!(
        codex.cost_usd_micros, None,
        "no reported cost is 'not reported', not zero"
    );
    let nobody = summary("nobody");
    assert_eq!(nobody.input_tokens, 0);
    assert_eq!(nobody.cost_usd_micros, None);
    assert_eq!(store.limit_snapshots("nobody").unwrap(), Vec::new());
    assert_eq!(store.session_usage_totals("nobody").unwrap(), Vec::new());
}

#[test]
fn limit_snapshots_are_one_per_account_and_window_and_never_go_back_in_time() {
    let (_dir, store) = open();
    assert_eq!(store.usage_account_ids().unwrap(), Vec::<String>::new());
    let snapshot =
        |account_id: &str, window: &str, used_percent: f64, captured_at: &str| LimitSnapshot {
            account_id: account_id.to_owned(),
            window: window.to_owned(),
            used_percent: Some(used_percent),
            resets_at: Some("2026-09-24T17:00:00Z".parse().unwrap()),
            captured_at: captured_at.parse().unwrap(),
        };
    for (account_id, window, used_percent, captured_at) in [
        ("claude-max", "five_hour", 10.0, "2026-09-24T12:00:00Z"),
        ("claude-max", "five_hour", 42.5, "2026-09-24T13:00:00Z"),
        // Out of order: an older capture must not overwrite the newer one already stored.
        ("claude-max", "five_hour", 99.0, "2026-09-24T12:30:00Z"),
        ("claude-max", "seven_day", 3.0, "2026-09-24T12:00:00Z"),
        ("codex", "primary", 2.0, "2026-09-24T12:00:00Z"),
    ] {
        store
            .record_limit_snapshot(&snapshot(account_id, window, used_percent, captured_at))
            .unwrap();
    }

    let claude = store.limit_snapshots("claude-max").unwrap();
    assert_eq!(claude.len(), 2, "one row per (account, window)");
    assert_eq!(claude[0].used_percent, Some(42.5));
    assert_eq!(store.limit_snapshots("codex").unwrap().len(), 1);
    assert_eq!(store.usage_account_ids().unwrap(), ["claude-max", "codex"]);
}

#[test]
fn session_totals_are_replaced_not_added_and_keep_an_unnamed_model() {
    let (_dir, mut store) = open();
    let totals = |input_tokens, model: Option<&str>| SessionModelUsage {
        model: model.map(str::to_owned),
        input_tokens,
        output_tokens: 10,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd_micros: Some(500),
    };
    store
        .set_session_usage_totals("sess-1", &[totals(100, Some("opus"))])
        .unwrap();
    // A later run of the same session reports the vendor's new cumulative totals, which must
    // replace the baseline wholesale rather than add to it.
    let later = [totals(120, Some("opus")), totals(5, None)];
    store.set_session_usage_totals("sess-1", &later).unwrap();
    assert_eq!(
        store.session_usage_totals("sess-1").unwrap(),
        [totals(5, None), totals(120, Some("opus"))]
    );
}
