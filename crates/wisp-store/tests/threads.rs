use rusqlite::Connection;
use uuid::Uuid;
use wisp_store::{RepoFields, RunFields, RunState, Store, StoreError, StoredEvent, WorktreeFields};

fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("wisp.sqlite3")).unwrap();
    (dir, store)
}

fn repo_fields(path: &str) -> RepoFields {
    RepoFields {
        name: path.rsplit('/').next().unwrap().to_owned(),
        path: path.to_owned(),
        scratch: false,
    }
}

fn run_fields(repo: Uuid) -> RunFields {
    RunFields {
        project_id: repo,
        prompt: "Fix the flaky attach test.".to_owned(),
        requested_account: None,
        policy: "workspaceWrite".to_owned(),
        backend: "claude".to_owned(),
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

fn state() -> RunState {
    RunState {
        status: "starting".to_owned(),
        account_id: "claude".to_owned(),
        ..RunState::default()
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
}

#[test]
fn the_scratch_entry_is_found_by_its_flag() {
    let (_dir, mut store) = open();
    store
        .add_repo(Uuid::now_v7(), &repo_fields("/Users/me/src/wisp"))
        .unwrap();
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
    let (thread, run, worktree) = store
        .create_thread_run(
            id,
            repo.id,
            &run_fields(repo.id),
            &state(),
            &worktree_fields(id),
        )
        .unwrap();
    assert_eq!(thread.id, id);
    assert_eq!(thread.repo_id, repo.id);
    assert!(!thread.archived);
    assert_eq!(run.fields.project_id, repo.id);
    assert_eq!(worktree.id, id);
    assert_eq!(store.list_runs(Some(repo.id)).unwrap(), vec![run]);

    let duplicate = store
        .create_thread_run(
            id,
            repo.id,
            &run_fields(repo.id),
            &state(),
            &worktree_fields(id),
        )
        .unwrap_err();
    assert!(matches!(duplicate, StoreError::IdConflict { .. }));
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
fn deleting_a_thread_removes_its_run_worktree_and_events_only() {
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
                &state(),
                &worktree_fields(id),
            )
            .unwrap();
    }
    for (seq, run) in [(1, kept), (2, gone), (3, gone)] {
        store
            .append_event(&StoredEvent {
                seq,
                time: "2026-09-26T12:00:00Z".parse().unwrap(),
                project_id: Some(repo.id),
                run_id: Some(run),
                kind: "agent.output".to_owned(),
                payload: "{}".to_owned(),
            })
            .unwrap();
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

    assert!(store.get_thread(kept).unwrap().is_some());
    assert!(store.get_run(kept).unwrap().is_some());
    assert_eq!(store.run_events(kept, 0, 10, 1 << 20).unwrap().0.len(), 1);
}

/// Two branches each added a migration: a database that has a newer version but is missing an
/// older one, as when #157's migration 8 lands after this one, still gets the older one.
#[test]
fn a_missing_migration_below_the_newest_still_applies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wisp.sqlite3");
    drop(Store::open(&path).unwrap());
    let versions = |conn: &Connection| -> Vec<i64> {
        conn.prepare("SELECT version FROM schema_version ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let conn = Connection::open(&path).unwrap();
    let all = versions(&conn);
    assert!(all.contains(&9), "{all:?}");
    conn.execute_batch("DELETE FROM schema_version WHERE version = 6; DROP TABLE role_defaults;")
        .unwrap();
    drop(conn);

    drop(Store::open(&path).unwrap());
    let conn = Connection::open(&path).unwrap();
    assert_eq!(versions(&conn), all);
    let restored: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'role_defaults'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(restored, 1);
}
