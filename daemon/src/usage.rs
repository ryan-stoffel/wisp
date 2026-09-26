//! Turns backend usage events into `wisp-store` rows.
//!
//! The runner (`crate::agents`) calls [`record_event`] once per event through the store's thread,
//! charged to whichever account the run is on at that point, which changes on
//! `Event::AccountFallback`.

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
        // `usage_totals` is empty only when the vendor's own totals are genuinely empty (a
        // fresh session that used nothing, in which case there is no baseline to lose), or when
        // `EventStream` synthesizes this event because the backend task died without ever
        // calling `EventSink::finish` (`EventStream::next` in `backend/mod.rs`). In the second case the
        // session may already have a real baseline recorded from before the crash, and replacing
        // it with nothing would make the next resume start from zero and double-count every
        // delta the vendor's cumulative total already carries. Skipping the write leaves the
        // last known baseline in place either way.
        Event::Finished { usage_totals, .. } if !usage_totals.is_empty() => {
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
        Event, EventSink, LimitStatus, LimitWindow, ModelUsage, Outcome, RunId, Usage,
    };

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("wisp.sqlite3")).unwrap();
        (dir, store)
    }

    fn usage(input: u64, output: u64) -> ModelUsage {
        ModelUsage {
            model: Some("claude-opus".to_owned()),
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                ..Usage::default()
            },
        }
    }

    fn finished(input: u64, output: u64) -> Event {
        Event::Finished {
            outcome: Outcome::Completed { result: None },
            usage_totals: vec![usage(input, output)],
        }
    }

    fn input_tokens(store: &Store) -> u64 {
        let far_past = "2020-01-01T00:00:00Z".parse().unwrap();
        let far_future = "2030-01-01T00:00:00Z".parse().unwrap();
        store
            .usage_summary("claude-max", far_past, far_future)
            .unwrap()
            .input_tokens
    }

    /// `SessionStarted` carries no usage of its own, two `Usage` deltas accumulate, a
    /// `RateLimit` snapshots the account's window, and `Finished` replaces the session's totals.
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
            Event::Usage(usage(100, 10)),
            Event::Usage(usage(50, 5)),
            Event::RateLimit(LimitWindow {
                window: "five_hour".to_owned(),
                duration_minutes: Some(300),
                used_percent: Some(12.5),
                status: LimitStatus::Allowed,
                resets_at: Some("2026-09-24T17:00:00Z".parse().unwrap()),
            }),
            finished(150, 15),
        ];
        for event in &events {
            record_event(&mut store, run_id, "claude-max", "sess-abc", event).unwrap();
        }

        assert_eq!(input_tokens(&store), 150, "two deltas accumulate");
        let limits = store.limit_snapshots("claude-max").unwrap();
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].used_percent, Some(12.5));
        let totals = store.session_usage_totals("sess-abc").unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].input_tokens, 150, "the run's final running total");
        assert_eq!(totals[0].output_tokens, 15);
    }

    /// A run that dies without reaching `EventSink::finish` gets a synthesized `Finished` with
    /// empty `usage_totals`. Recording it must not wipe the baseline an earlier run established,
    /// or the next resume would double-count the vendor's cumulative total.
    #[tokio::test]
    async fn a_run_that_dies_without_finishing_does_not_wipe_the_resume_baseline() {
        let (_dir, mut store) = open();
        let mut record = |event: &Event| {
            record_event(&mut store, RunId::generate(), "claude-max", "sess", event).unwrap();
        };

        record(&Event::Usage(usage(100, 10)));
        record(&finished(100, 10));

        // The exact production path: drop the sink and read the synthesized `Finished`.
        let (sink, mut stream) = EventSink::channel(8, Vec::new());
        drop(sink);
        record(&stream.next().await.expect("a synthesized Finished"));

        // The resume's delta is only what is new since 100; its total is the vendor's 120.
        record(&Event::Usage(usage(20, 2)));
        record(&finished(120, 12));

        assert_eq!(input_tokens(&store), 120, "never double-counted");
        let totals = store.session_usage_totals("sess").unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].input_tokens, 120);
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
            Event::Usage(usage(0, 0)),
            Event::FollowUpDropped {
                turn_id: crate::backend::TurnId::generate(),
            },
        ] {
            record_event(&mut store, run_id, "claude-max", "sess", &event).unwrap();
        }
        assert_eq!(store.usage_account_ids().unwrap(), Vec::<String>::new());
    }
}
