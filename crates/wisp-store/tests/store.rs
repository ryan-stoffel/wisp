use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use rusqlite::Connection;
use uuid::Uuid;
use wisp_store::{ProjectFields, Store, StoreError};

fn temp_db_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("wisp.sqlite3");
    (dir, path)
}

fn sample_fields() -> ProjectFields {
    ProjectFields {
        name: "wisp".to_string(),
        repo_path: "/Users/ryan/dev/wisp".to_string(),
        host: "macbook".to_string(),
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
    assert_eq!(project.host, sample_fields().host);
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
    updated_fields.host = "mac-mini".to_string();
    let updated = store
        .update_project(id, &updated_fields)
        .expect("update of an existing project should succeed");

    assert_eq!(updated.host, "mac-mini");
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
            "INSERT INTO projects (id, name, repo_path, host, created_at, updated_at)
             VALUES ('01978c1e-70a0-7c3d-9b1a-000000000000', 'n', '/r', 'h', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
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
