use rusqlite::Connection;
use uuid::Uuid;
use wisp_store::{RunFields, RunState, Store, StoreError, StoredEvent, WorktreeFields};

fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let store = Store::open(dir.path().join("wisp.sqlite3")).expect("open");
    (dir, store)
}

fn fields(project_id: Uuid) -> RunFields {
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

#[test]
fn a_run_is_created_once_read_back_and_updated() {
    let (_dir, store) = open();
    let (project, id) = (Uuid::now_v7(), Uuid::now_v7());
    let created = store.create_run(id, &fields(project), &starting()).unwrap();
    assert_eq!(created.id, id);
    assert_eq!(created.fields, fields(project));
    assert_eq!(created.state, starting());
    assert_eq!(store.get_run(id).unwrap(), Some(created.clone()));

    assert!(matches!(
        store.create_run(id, &fields(project), &starting()),
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
    };
    let updated = store.update_run(id, &finished).unwrap();
    assert_eq!(updated.state, finished);
    assert_eq!(updated.fields, fields(project), "the request never changes");
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
fn runs_list_oldest_first_and_by_project() {
    let (_dir, store) = open();
    let (one, two) = (Uuid::now_v7(), Uuid::now_v7());
    let first = store
        .create_run(Uuid::now_v7(), &fields(one), &starting())
        .unwrap();
    let second = store
        .create_run(Uuid::now_v7(), &fields(two), &starting())
        .unwrap();
    let third = store
        .create_run(Uuid::now_v7(), &fields(one), &starting())
        .unwrap();
    let ids = |runs: Vec<wisp_store::Run>| runs.into_iter().map(|run| run.id).collect::<Vec<_>>();
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
fn the_log_id_is_stored_once() {
    let (dir, store) = open();
    let first = Uuid::now_v7();
    assert_eq!(store.event_log_id(first).unwrap(), first);
    assert_eq!(store.event_log_id(Uuid::now_v7()).unwrap(), first);
    drop(store);
    let reopened = Store::open(dir.path().join("wisp.sqlite3")).unwrap();
    assert_eq!(reopened.event_log_id(Uuid::now_v7()).unwrap(), first);
}

#[test]
fn events_append_and_read_back_by_head_tail_and_run() {
    let (_dir, store) = open();
    store.relax_sync().unwrap();
    assert_eq!(store.event_head().unwrap(), 0);
    assert!(store.latest_events(10).unwrap().is_empty());

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
    assert_eq!(store.latest_events(2).unwrap(), events[3..]);
    assert_eq!(store.latest_events(100).unwrap(), events);

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

fn worktree_fields() -> WorktreeFields {
    WorktreeFields {
        repo_path: "/src/app".to_owned(),
        path: "/data/worktrees/app/run".to_owned(),
        branch: "wisp/abcd1234".to_owned(),
        base: "abc".to_owned(),
        git_dir: "/src/app/.git/worktrees/run".to_owned(),
    }
}

#[test]
fn a_run_and_its_worktree_are_created_together_or_not_at_all() {
    let (_dir, mut store) = open();
    let id = Uuid::now_v7();
    let (run, worktree) = store
        .create_run_with_worktree(id, &fields(Uuid::now_v7()), &starting(), &worktree_fields())
        .unwrap();
    assert_eq!(store.get_run(id).unwrap(), Some(run));
    assert_eq!(store.get_worktree(id).unwrap(), Some(worktree));

    // A worktree row already there makes the run's insert roll back with it.
    let taken = Uuid::now_v7();
    store.create_worktree(taken, &worktree_fields()).unwrap();
    assert!(matches!(
        store.create_run_with_worktree(
            taken,
            &fields(Uuid::now_v7()),
            &starting(),
            &worktree_fields()
        ),
        Err(StoreError::IdConflict { .. })
    ));
    assert_eq!(store.get_run(taken).unwrap(), None);

    // A run row already there leaves no new worktree row behind.
    let run_only = Uuid::now_v7();
    store
        .create_run(run_only, &fields(Uuid::now_v7()), &starting())
        .unwrap();
    assert!(
        store
            .create_run_with_worktree(
                run_only,
                &fields(Uuid::now_v7()),
                &starting(),
                &worktree_fields()
            )
            .is_err()
    );
    assert_eq!(store.get_worktree(run_only).unwrap(), None);
}

#[test]
fn resetting_the_log_id_makes_the_next_one_new() {
    let (_dir, store) = open();
    let first = store.event_log_id(Uuid::now_v7()).unwrap();
    store.reset_event_log_id().unwrap();
    let second = Uuid::now_v7();
    assert_eq!(store.event_log_id(second).unwrap(), second);
    assert_ne!(second, first);
}

#[test]
fn a_version_6_database_gains_runs_events_and_worktree_git_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wisp.sqlite3");
    let id = Uuid::now_v7();
    {
        let mut store = Store::open(&path).unwrap();
        store
            .create_worktree(
                id,
                &WorktreeFields {
                    repo_path: "/src/app".to_owned(),
                    path: "/data/worktrees/app/run".to_owned(),
                    branch: "wisp/abcd1234".to_owned(),
                    base: "abc".to_owned(),
                    git_dir: "/src/app/.git/worktrees/run".to_owned(),
                },
            )
            .unwrap();
    }
    // Roll the database back to what #119 left on develop: schema 6, no runs, events, or
    // git_dir column.
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE runs; DROP TABLE log_meta; DROP TABLE events;
             ALTER TABLE worktrees DROP COLUMN git_dir;
             DELETE FROM schema_version WHERE version = 7;",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    let worktree = store.get_worktree(id).unwrap().unwrap();
    assert_eq!(worktree.git_dir, "", "an older row has no pinned git dir");
    let run = store
        .create_run(Uuid::now_v7(), &fields(Uuid::now_v7()), &starting())
        .unwrap();
    assert_eq!(store.list_runs(None).unwrap(), [run]);
    store.append_event(&event(1, None)).unwrap();
    assert_eq!(store.event_head().unwrap(), 1);
}
