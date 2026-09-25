//! Per-account usage: deltas, limit snapshots, and session totals, against a real store.

use std::path::PathBuf;

use uuid::Uuid;
use wisp_store::{LimitSnapshot, SessionModelUsage, Store, UsageDelta};

fn temp_db_path() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create temp dir");
    let path = dir.path().join("wisp.sqlite3");
    (dir, path)
}

fn delta(run_id: Uuid, account_id: &str, at: &str, input: u64) -> UsageDelta {
    UsageDelta {
        run_id,
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

#[test]
fn deltas_accumulate_over_a_range() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open");
    let run = Uuid::now_v7();
    store
        .record_usage_delta(&delta(run, "claude-max", "2026-09-24T10:00:00Z", 100))
        .expect("record");
    store
        .record_usage_delta(&delta(run, "claude-max", "2026-09-24T11:00:00Z", 50))
        .expect("record");

    let summary = store
        .usage_summary(
            "claude-max",
            "2026-09-24T00:00:00Z".parse().unwrap(),
            "2026-09-25T00:00:00Z".parse().unwrap(),
        )
        .expect("summary");
    assert_eq!(summary.input_tokens, 150);
    assert_eq!(summary.output_tokens, 150);
    assert_eq!(summary.cost_usd_micros, Some(1_500));
}

/// The day boundary is `[start, end)`: a delta exactly at the end of a day belongs to the next
/// day, and one exactly at the start of a day belongs to it.
#[test]
fn deltas_are_bucketed_correctly_across_a_day_boundary() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open");
    let run = Uuid::now_v7();
    store
        .record_usage_delta(&delta(run, "acct", "2026-09-23T23:59:59Z", 1))
        .expect("record");
    store
        .record_usage_delta(&delta(run, "acct", "2026-09-24T00:00:00Z", 2))
        .expect("record");
    store
        .record_usage_delta(&delta(run, "acct", "2026-09-24T23:59:59.999999999Z", 4))
        .expect("record");
    store
        .record_usage_delta(&delta(run, "acct", "2026-09-25T00:00:00Z", 8))
        .expect("record");

    let today = store
        .usage_summary(
            "acct",
            "2026-09-24T00:00:00Z".parse().unwrap(),
            "2026-09-25T00:00:00Z".parse().unwrap(),
        )
        .expect("summary");
    assert_eq!(today.input_tokens, 2 + 4, "half-open range: [start, end)");

    let week = store
        .usage_summary(
            "acct",
            "2026-09-21T00:00:00Z".parse().unwrap(),
            "2026-09-25T00:00:00Z".parse().unwrap(),
        )
        .expect("summary");
    assert_eq!(
        week.input_tokens,
        1 + 2 + 4,
        "the week also excludes the 25th"
    );
}

#[test]
fn cost_is_not_reported_when_no_delta_in_range_reported_one() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open");
    let run = Uuid::now_v7();
    let mut no_cost = delta(run, "codex", "2026-09-24T10:00:00Z", 100);
    no_cost.cost_usd_micros = None;
    store.record_usage_delta(&no_cost).expect("record");

    let summary = store
        .usage_summary(
            "codex",
            "2026-09-24T00:00:00Z".parse().unwrap(),
            "2026-09-25T00:00:00Z".parse().unwrap(),
        )
        .expect("summary");
    assert_eq!(summary.input_tokens, 100, "tokens are still counted");
    assert_eq!(
        summary.cost_usd_micros, None,
        "no reported cost is 'not reported', not zero"
    );
}

#[test]
fn an_unknown_account_reads_back_safe_defaults() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open");
    let summary = store
        .usage_summary(
            "nobody",
            "2026-09-24T00:00:00Z".parse().unwrap(),
            "2026-09-25T00:00:00Z".parse().unwrap(),
        )
        .expect("summary");
    assert_eq!(summary.input_tokens, 0);
    assert_eq!(summary.cost_usd_micros, None);
    assert_eq!(store.limit_snapshots("nobody").expect("limits"), Vec::new());
    assert_eq!(
        store.session_usage_totals("nobody").expect("totals"),
        Vec::new()
    );
    assert_eq!(
        store.usage_account_ids().expect("ids"),
        Vec::<String>::new()
    );
}

#[test]
fn a_newer_limit_snapshot_replaces_the_old_one_but_an_older_one_does_not() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open");
    let snapshot = |used_percent: f64, captured_at: &str| LimitSnapshot {
        account_id: "claude-max".to_owned(),
        window: "five_hour".to_owned(),
        used_percent: Some(used_percent),
        resets_at: Some("2026-09-24T17:00:00Z".parse().unwrap()),
        captured_at: captured_at.parse().unwrap(),
    };
    store
        .record_limit_snapshot(&snapshot(10.0, "2026-09-24T12:00:00Z"))
        .expect("record");
    store
        .record_limit_snapshot(&snapshot(42.5, "2026-09-24T13:00:00Z"))
        .expect("record");
    // Out of order: an older capture must not overwrite the newer one already stored.
    store
        .record_limit_snapshot(&snapshot(99.0, "2026-09-24T12:30:00Z"))
        .expect("record");

    let snapshots = store.limit_snapshots("claude-max").expect("limits");
    assert_eq!(snapshots.len(), 1, "one row per (account, window)");
    assert_eq!(snapshots[0].used_percent, Some(42.5));
}

#[test]
fn each_account_and_window_has_its_own_snapshot() {
    let (_dir, path) = temp_db_path();
    let store = Store::open(&path).expect("open");
    store
        .record_limit_snapshot(&LimitSnapshot {
            account_id: "claude-max".to_owned(),
            window: "five_hour".to_owned(),
            used_percent: Some(1.0),
            resets_at: None,
            captured_at: "2026-09-24T12:00:00Z".parse().unwrap(),
        })
        .expect("record");
    store
        .record_limit_snapshot(&LimitSnapshot {
            account_id: "claude-max".to_owned(),
            window: "seven_day".to_owned(),
            used_percent: Some(3.0),
            resets_at: None,
            captured_at: "2026-09-24T12:00:00Z".parse().unwrap(),
        })
        .expect("record");
    store
        .record_limit_snapshot(&LimitSnapshot {
            account_id: "codex".to_owned(),
            window: "primary".to_owned(),
            used_percent: Some(2.0),
            resets_at: None,
            captured_at: "2026-09-24T12:00:00Z".parse().unwrap(),
        })
        .expect("record");
    assert_eq!(
        store.limit_snapshots("claude-max").expect("limits").len(),
        2
    );
    assert_eq!(store.limit_snapshots("codex").expect("limits").len(), 1);
    assert_eq!(
        store.usage_account_ids().expect("ids"),
        ["claude-max".to_owned(), "codex".to_owned()]
    );
}

#[test]
fn resuming_a_session_replaces_its_totals_instead_of_adding_to_them() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let first = SessionModelUsage {
        model: Some("opus".to_owned()),
        input_tokens: 100,
        output_tokens: 10,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd_micros: Some(500),
    };
    store
        .set_session_usage_totals("sess-1", std::slice::from_ref(&first))
        .expect("set totals");
    assert_eq!(
        store.session_usage_totals("sess-1").expect("totals"),
        [first]
    );

    // A later run of the same session reports the vendor's new cumulative totals, which must
    // replace the baseline wholesale rather than add to it.
    let second = SessionModelUsage {
        model: Some("opus".to_owned()),
        input_tokens: 120,
        output_tokens: 15,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd_micros: Some(600),
    };
    store
        .set_session_usage_totals("sess-1", std::slice::from_ref(&second))
        .expect("set totals");
    assert_eq!(
        store.session_usage_totals("sess-1").expect("totals"),
        [second],
        "must not be 100+120 etc.: totals replace, they never add"
    );
}

#[test]
fn session_totals_with_no_model_use_the_sentinel_and_read_back_as_none() {
    let (_dir, path) = temp_db_path();
    let mut store = Store::open(&path).expect("open");
    let unnamed = SessionModelUsage {
        model: None,
        input_tokens: 5,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cost_usd_micros: None,
    };
    store
        .set_session_usage_totals("sess-2", std::slice::from_ref(&unnamed))
        .expect("set totals");
    assert_eq!(
        store.session_usage_totals("sess-2").expect("totals"),
        [unnamed]
    );
}
