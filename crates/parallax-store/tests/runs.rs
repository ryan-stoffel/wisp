use parallax_store::{
    RunAccept, RunFields, RunState, Store, StoreError, StoredEvent, StoredImage, WakeState,
    WorktreeFields,
};
use rusqlite::Connection;
use uuid::Uuid;

fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let store = Store::open(dir.path().join("parallax.sqlite3")).expect("open");
    (dir, store)
}

fn fields(project_id: Uuid) -> RunFields {
    RunFields {
        project_id,
        prompt: "Add a README".to_owned(),
        requested_account: Some(r#"{"kind":"subscription","backend":"claude"}"#.to_owned()),
        policy: "workspaceWrite".to_owned(),
        backend: "claude".to_owned(),
        coordinator_thread: None,
        parent: None,
        model: Some("opus".to_owned()),
        effort: Some("high".to_owned()),
        permission: None,
        context_window: None,
        fast: None,
        approvals: false,
        checkout: false,
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
fn a_run_keeps_the_coordinator_thread_that_started_it() {
    let (_dir, store) = open();
    let (project, id, thread) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let tagged = RunFields {
        coordinator_thread: Some(thread),
        ..fields(project)
    };
    store.create_run(id, &tagged, &starting()).unwrap();
    let read = store.get_run(id).unwrap().unwrap();
    assert_eq!(read.fields.coordinator_thread, Some(thread));
    let listed = store.list_runs(Some(project)).unwrap();
    assert_eq!(listed[0].fields, tagged);
}

/// RYA-222: whether a run forwards its permission requests is kept with it, for every launch.
#[test]
fn a_run_keeps_whether_it_forwards_permission_requests() {
    let (_dir, store) = open();
    let (project, asking, quiet) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let forwarding = RunFields {
        approvals: true,
        ..fields(project)
    };
    store.create_run(asking, &forwarding, &starting()).unwrap();
    store
        .create_run(quiet, &fields(project), &starting())
        .unwrap();
    assert!(store.get_run(asking).unwrap().unwrap().fields.approvals);
    assert!(!store.get_run(quiet).unwrap().unwrap().fields.approvals);
}

/// A thread in the repository's own checkout keeps that, for every launch.
#[test]
fn a_run_keeps_whether_it_works_in_the_current_checkout() {
    let (_dir, store) = open();
    let (project, checkout, worktree) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());
    let in_place = RunFields {
        checkout: true,
        ..fields(project)
    };
    store.create_run(checkout, &in_place, &starting()).unwrap();
    store
        .create_run(worktree, &fields(project), &starting())
        .unwrap();
    assert!(store.get_run(checkout).unwrap().unwrap().fields.checkout);
    assert!(!store.get_run(worktree).unwrap().unwrap().fields.checkout);
}

/// A run keeps its context window and fast mode, and `set_run_options` replaces them with its
/// backend and other options.
#[test]
fn a_run_keeps_its_context_window_and_fast_mode() {
    let (_dir, store) = open();
    let (project, id) = (Uuid::now_v7(), Uuid::now_v7());
    let asked = RunFields {
        context_window: Some(1_000_000),
        fast: Some(true),
        ..fields(project)
    };
    store.create_run(id, &asked, &starting()).unwrap();
    assert_eq!(store.get_run(id).unwrap().unwrap().fields, asked);
    let moved = RunFields {
        backend: "codex".to_owned(),
        model: Some("gpt-5.5".to_owned()),
        context_window: Some(272_000),
        fast: Some(false),
        ..asked
    };
    let row = store.set_run_options(id, &moved).unwrap();
    assert_eq!(row.fields, moved);
    assert_eq!(store.get_run(id).unwrap().unwrap().fields, moved);
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
        accept: None,
        pull_requests: vec![
            "https://github.com/me/app/pull/7".to_owned(),
            "https://github.com/me/app/pull/9".to_owned(),
        ],
        auto_resume: Some(false),
        resume_at: Some("2026-10-03T18:00:00Z".parse().unwrap()),
        resume_tries: 2,
    };
    let updated = store.update_run(id, &finished).unwrap();
    assert_eq!(updated.state, finished);
    assert_eq!(store.get_run(id).unwrap().unwrap().state, finished);
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
fn accepting_a_run_records_the_merge_and_drops_its_worktree_together() {
    let (_dir, mut store) = open();
    let (project, id) = (Uuid::now_v7(), Uuid::now_v7());
    let worktree = WorktreeFields {
        repo_path: "/src/app".to_owned(),
        path: "/data/worktrees/app/run".to_owned(),
        branch: "parallax/abcd1234".to_owned(),
        base: "abc".to_owned(),
        git_dir: "/src/app/.git/worktrees/run".to_owned(),
        base_dirty: false,
    };
    store
        .create_run_with_worktree(id, &fields(project), &starting(), &worktree)
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
        .create_run(Uuid::now_v7(), &fields(one), &starting())
        .unwrap();
    let second = store
        .create_run(Uuid::now_v7(), &fields(two), &starting())
        .unwrap();
    let third = store
        .create_run(Uuid::now_v7(), &fields(one), &starting())
        .unwrap();
    let ids =
        |runs: Vec<parallax_store::Run>| runs.into_iter().map(|run| run.id).collect::<Vec<_>>();
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
    let reopened = Store::open(dir.path().join("parallax.sqlite3")).unwrap();
    assert_eq!(reopened.event_log_id(Uuid::now_v7()).unwrap(), first);
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

fn worktree_fields() -> WorktreeFields {
    WorktreeFields {
        repo_path: "/src/app".to_owned(),
        path: "/data/worktrees/app/run".to_owned(),
        branch: "parallax/abcd1234".to_owned(),
        base: "abc".to_owned(),
        git_dir: "/src/app/.git/worktrees/run".to_owned(),
        base_dirty: false,
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

/// PLX-338: `project/delete` removes each of a Project's runs with every row kept for it.
#[test]
fn deleting_a_run_removes_its_worktree_events_turns_images_and_wakes_only() {
    let (_dir, mut store) = open();
    let project = Uuid::now_v7();
    let [kept, gone] = [Uuid::now_v7(), Uuid::now_v7()];
    let image = Uuid::now_v7();
    for (seq, id) in [(1, kept), (2, gone)] {
        store
            .create_run_with_worktree(id, &fields(project), &starting(), &worktree_fields())
            .unwrap();
        store.append_event(&event(seq, Some(id))).unwrap();
        store.record_turn(id, Uuid::now_v7(), "carry on").unwrap();
        let stored = StoredImage {
            media_type: "image/png".to_owned(),
            data: "iVBORw0KGgo=".to_owned(),
        };
        store.add_images(id, &[(image, stored)]).unwrap();
        let wakes = WakeState {
            in_a_row: 2,
            paused: true,
        };
        store.set_wake_state(id, wakes).unwrap();
    }

    assert!(store.delete_run(gone).unwrap());
    assert!(
        !store.delete_run(gone).unwrap(),
        "deleting again does nothing"
    );
    assert_eq!(store.get_run(gone).unwrap(), None);
    assert_eq!(store.get_worktree(gone).unwrap(), None);
    assert!(store.run_events(gone, 0, 10, 1 << 20).unwrap().0.is_empty());
    assert_eq!(store.run_turns(gone).unwrap(), []);
    assert_eq!(store.image(gone, image).unwrap(), None);
    assert_eq!(store.wake_state(gone).unwrap(), WakeState::default());

    assert!(store.get_run(kept).unwrap().is_some());
    assert!(store.get_worktree(kept).unwrap().is_some());
    assert_eq!(store.run_events(kept, 0, 10, 1 << 20).unwrap().0.len(), 1);
    assert_eq!(store.run_turns(kept).unwrap().len(), 1);
    assert!(store.image(kept, image).unwrap().is_some());
    assert!(store.wake_state(kept).unwrap().paused);
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
    let path = dir.path().join("parallax.sqlite3");
    let id = Uuid::now_v7();
    {
        let mut store = Store::open(&path).unwrap();
        store
            .create_worktree(
                id,
                &WorktreeFields {
                    repo_path: "/src/app".to_owned(),
                    path: "/data/worktrees/app/run".to_owned(),
                    branch: "parallax/abcd1234".to_owned(),
                    base: "abc".to_owned(),
                    git_dir: "/src/app/.git/worktrees/run".to_owned(),
                    base_dirty: false,
                },
            )
            .unwrap();
    }
    // Roll the database back to what #119 left on develop: schema 6, no runs, events, git_dir or
    // base_dirty columns, turns table (#190's migration 10; dropping `runs` already undoes #157's
    // migration 8 columns on it, since they're columns of the table this drops wholesale), the
    // normal threads tables (#110's migration 9), the wakes table (RYA-178's migration 14), the
    // images table (RYA-191's migration 15), the project icon columns (RYA-227's migration 16),
    // the host settings table (PLX-371's migration 24), or the inbox table (PLX-401's migration
    // 25).
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "DROP TABLE runs; DROP TABLE log_meta; DROP TABLE events; DROP TABLE turns;
             DROP TABLE threads; DROP TABLE repos; DROP TABLE wakes; DROP TABLE images;
             DROP TABLE host_settings; DROP TABLE inbox;
             ALTER TABLE worktrees DROP COLUMN git_dir;
             ALTER TABLE worktrees DROP COLUMN base_dirty;
             ALTER TABLE projects DROP COLUMN icon_name;
             ALTER TABLE projects DROP COLUMN icon_color;
             ALTER TABLE projects DROP COLUMN icon_image_type;
             ALTER TABLE projects DROP COLUMN icon_image_data;
             DELETE FROM schema_version WHERE version >= 7;",
        )
        .unwrap();
    }
    let store = Store::open(&path).unwrap();
    let worktree = store.get_worktree(id).unwrap().unwrap();
    assert_eq!(worktree.git_dir, "", "an older row has no pinned git dir");
    assert!(!worktree.base_dirty, "an older row is never flagged dirty");
    let run = store
        .create_run(Uuid::now_v7(), &fields(Uuid::now_v7()), &starting())
        .unwrap();
    assert_eq!(store.list_runs(None).unwrap(), [run]);
    store.append_event(&event(1, None)).unwrap();
    assert_eq!(store.event_head().unwrap(), 1);
}
