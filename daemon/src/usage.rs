//! Turns backend usage events (0004, #113) into `wisp-store` rows (#120).
//!
//! [`record_event`] is a plain, synchronous function over `&mut wisp_store::Store`, the same
//! shape [`crate::methods::project`] uses for `project/create`. A future runner (M3) will call it
//! once per event through the store's single-thread owner
//! ([`crate::store::StoreHandle::run`](../store/struct.StoreHandle.html#method.run)), exactly the
//! way `methods::usage::get` already does for reads below: queued as another store job, so
//! recording never blocks whatever is forwarding the run's event stream onward. Until that runner
//! exists, this module is exercised directly with fixture events.

use jiff::Timestamp;
use wisp_store::{LimitSnapshot, SessionModelUsage, Store, StoreError, UsageDelta};

use crate::backend::{Event, ModelUsage, RunId};

/// Turns one backend event into store writes for `run_id`, charged to `account_id`, in
/// `session_id`'s vendor session (from that run's earlier `Event::SessionStarted`). Every other
/// event kind is a no-op.
///
/// # Errors
///
/// Returns a database error.
pub fn record_event(
    store: &mut Store,
    run_id: RunId,
    account_id: &str,
    session_id: &str,
    event: &Event,
) -> Result<(), StoreError> {
    match event {
        Event::Usage(delta) if !delta.usage.is_zero() => {
            store.record_usage_delta(&usage_delta(run_id, account_id, delta))?;
        }
        Event::RateLimit(window) => {
            store.record_limit_snapshot(&LimitSnapshot {
                account_id: account_id.to_owned(),
                window: window.window.clone(),
                used_percent: window.used_percent,
                resets_at: window.resets_at,
                captured_at: Timestamp::now(),
            })?;
        }
        Event::Finished { usage_totals, .. } => {
            let totals: Vec<_> = usage_totals.iter().map(session_total).collect();
            store.set_session_usage_totals(session_id, &totals)?;
        }
        _ => {}
    }
    Ok(())
}

fn usage_delta(run_id: RunId, account_id: &str, delta: &ModelUsage) -> UsageDelta {
    UsageDelta {
        run_id: run_id.into(),
        account_id: account_id.to_owned(),
        model: delta.model.clone(),
        input_tokens: delta.usage.input_tokens,
        output_tokens: delta.usage.output_tokens,
        cache_read_tokens: delta.usage.cache_read_tokens,
        cache_write_tokens: delta.usage.cache_write_tokens,
        cost_usd_micros: delta.usage.cost_usd_micros,
        at: Timestamp::now(),
    }
}

fn session_total(total: &ModelUsage) -> SessionModelUsage {
    SessionModelUsage {
        model: total.model.clone(),
        input_tokens: total.usage.input_tokens,
        output_tokens: total.usage.output_tokens,
        cache_read_tokens: total.usage.cache_read_tokens,
        cache_write_tokens: total.usage.cache_write_tokens,
        cost_usd_micros: total.usage.cost_usd_micros,
    }
}

#[cfg(test)]
mod tests {
    use wisp_store::Store;

    use super::record_event;
    use crate::backend::{
        Event, FailureKind, LimitStatus, LimitWindow, ModelUsage, Outcome, RunId, Usage,
    };

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wisp.sqlite3");
        let store = Store::open(&path).unwrap();
        (dir, store)
    }

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        }
    }

    /// A run resuming an existing session: `SessionStarted` carries no usage of its own, two
    /// `Usage` deltas accumulate, a `RateLimit` snapshots the account's window, and `Finished`
    /// replaces the session's running totals.
    #[test]
    fn a_fixture_run_records_deltas_a_limit_snapshot_and_session_totals() {
        let (_dir, mut store) = open();
        let run_id = RunId::generate();
        let events = [
            Event::SessionStarted {
                session_id: "sess-abc".to_owned(),
                model: Some("claude-opus".to_owned()),
                api_key_source: None,
            },
            Event::Usage(ModelUsage {
                model: Some("claude-opus".to_owned()),
                usage: usage(100, 10),
            }),
            Event::Usage(ModelUsage {
                model: Some("claude-opus".to_owned()),
                usage: usage(50, 5),
            }),
            Event::RateLimit(LimitWindow {
                window: "five_hour".to_owned(),
                duration_minutes: Some(300),
                used_percent: Some(12.5),
                status: LimitStatus::Allowed,
                resets_at: Some("2026-09-24T17:00:00Z".parse().unwrap()),
            }),
            Event::Finished {
                outcome: Outcome::Completed { result: None },
                usage_totals: vec![ModelUsage {
                    model: Some("claude-opus".to_owned()),
                    usage: usage(150, 15),
                }],
            },
        ];

        for event in &events {
            record_event(&mut store, run_id, "claude-max", "sess-abc", event).unwrap();
        }

        let today = store
            .usage_summary(
                "claude-max",
                jiff::Timestamp::now() - jiff::Span::new().hours(1),
                jiff::Timestamp::now() + jiff::Span::new().hours(1),
            )
            .unwrap();
        assert_eq!(today.input_tokens, 150, "two deltas accumulate");
        assert_eq!(today.output_tokens, 15);

        let limits = store.limit_snapshots("claude-max").unwrap();
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].used_percent, Some(12.5));

        let totals = store.session_usage_totals("sess-abc").unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].input_tokens, 150, "the run's final running total");
    }

    /// A resumed session's second run starts from a `Resume` baseline (0004/#113), so its own
    /// `Usage` deltas are already the *new* amount only; recording them must not add the
    /// baseline in a second time when the run's `Finished` replaces the session's totals.
    #[test]
    fn resuming_a_session_does_not_double_count_the_baseline() {
        let (_dir, mut store) = open();
        let run_id = RunId::generate();

        // First run: session starts fresh.
        record_event(
            &mut store,
            run_id,
            "claude-max",
            "sess-resume",
            &Event::Usage(ModelUsage {
                model: None,
                usage: usage(1000, 100),
            }),
        )
        .unwrap();
        record_event(
            &mut store,
            run_id,
            "claude-max",
            "sess-resume",
            &Event::Finished {
                outcome: Outcome::Completed { result: None },
                usage_totals: vec![ModelUsage {
                    model: None,
                    usage: usage(1000, 100),
                }],
            },
        )
        .unwrap();

        // Second run resumes the session. The backend's `EventSink` (#113) already turns the
        // vendor's cumulative total into a delta of only what is new, so this event carries just
        // 200 more input tokens, and `Finished` reports the session's new grand total of 1200.
        let resumed_run = RunId::generate();
        record_event(
            &mut store,
            resumed_run,
            "claude-max",
            "sess-resume",
            &Event::Usage(ModelUsage {
                model: None,
                usage: usage(200, 20),
            }),
        )
        .unwrap();
        record_event(
            &mut store,
            resumed_run,
            "claude-max",
            "sess-resume",
            &Event::Finished {
                outcome: Outcome::Completed { result: None },
                usage_totals: vec![ModelUsage {
                    model: None,
                    usage: usage(1200, 120),
                }],
            },
        )
        .unwrap();

        let far_past = "2020-01-01T00:00:00Z".parse().unwrap();
        let far_future = "2030-01-01T00:00:00Z".parse().unwrap();
        let summary = store
            .usage_summary("claude-max", far_past, far_future)
            .unwrap();
        assert_eq!(
            summary.input_tokens, 1200,
            "deltas from both runs, not the baseline counted twice"
        );

        let totals = store.session_usage_totals("sess-resume").unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(
            totals[0].input_tokens, 1200,
            "the session's stored baseline is the vendor's real cumulative total"
        );
    }

    #[test]
    fn events_with_no_usage_or_limit_information_are_ignored() {
        let (_dir, mut store) = open();
        let run_id = RunId::generate();
        for event in [
            Event::TextDelta {
                message_id: None,
                text: "hi".to_owned(),
            },
            Event::Usage(ModelUsage {
                model: None,
                usage: Usage::default(),
            }),
            Event::FollowUpDropped {
                turn_id: crate::backend::TurnId::generate(),
            },
        ] {
            record_event(&mut store, run_id, "claude-max", "sess", &event).unwrap();
        }
        assert_eq!(store.usage_account_ids().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn an_unknown_account_has_no_effect_on_others() {
        let (_dir, mut store) = open();
        let run_id = RunId::generate();
        record_event(
            &mut store,
            run_id,
            "claude-max",
            "sess",
            &Event::Usage(ModelUsage {
                model: None,
                usage: usage(10, 1),
            }),
        )
        .unwrap();

        let far_past = "2020-01-01T00:00:00Z".parse().unwrap();
        let far_future = "2030-01-01T00:00:00Z".parse().unwrap();
        let unknown = store.usage_summary("nobody", far_past, far_future).unwrap();
        assert_eq!(unknown.input_tokens, 0);
        assert_eq!(unknown.cost_usd_micros, None);

        let known = store
            .usage_summary("claude-max", far_past, far_future)
            .unwrap();
        assert_eq!(known.input_tokens, 10);
    }

    #[test]
    fn a_crash_failure_still_leaves_recorded_deltas_intact() {
        let (_dir, mut store) = open();
        let run_id = RunId::generate();
        record_event(
            &mut store,
            run_id,
            "claude-max",
            "sess",
            &Event::Usage(ModelUsage {
                model: None,
                usage: usage(10, 1),
            }),
        )
        .unwrap();
        record_event(
            &mut store,
            run_id,
            "claude-max",
            "sess",
            &Event::Finished {
                outcome: Outcome::Failed(crate::backend::Failure {
                    failure: FailureKind::Crashed,
                    message: "boom".to_owned(),
                    exit: None,
                    stderr_tail: None,
                }),
                usage_totals: vec![ModelUsage {
                    model: None,
                    usage: usage(10, 1),
                }],
            },
        )
        .unwrap();

        let far_past = "2020-01-01T00:00:00Z".parse().unwrap();
        let far_future = "2030-01-01T00:00:00Z".parse().unwrap();
        assert_eq!(
            store
                .usage_summary("claude-max", far_past, far_future)
                .unwrap()
                .input_tokens,
            10
        );
        assert_eq!(store.session_usage_totals("sess").unwrap().len(), 1);
    }
}
