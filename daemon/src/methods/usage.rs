//! `usage/get`.

use jiff::{ToSpan, Zoned};
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{AccountUsage, UsageGetParams, UsageGetResult, UsageLimitWindow, UsagePeriod};
use wisp_store::{LimitSnapshot, Store, StoreError};

use super::Context;
use crate::store::store_error;

/// Every account wispd has recorded usage or limits for, with today's and this week's sums
/// (local time on this host) and the latest limit windows.
pub(crate) async fn get(
    context: &Context,
    _: UsageGetParams,
) -> Result<UsageGetResult, ErrorObject> {
    let now = Zoned::now();
    let (day_start, week_start) = local_bounds(&now).map_err(ErrorObject::internal_error)?;
    let until = now.timestamp();
    context
        .daemon
        .store
        .run(&context.cancel, move |store| {
            usage_report(store, day_start, week_start, until).map_err(|error| store_error(&error))
        })
        .await
}

/// Gathers every known account's usage report from the store. A plain, synchronous function so it
/// can run directly on the store's thread (see [`get`]) and be tested against a real [`Store`]
/// with no async runtime.
fn usage_report(
    store: &Store,
    day_start: jiff::Timestamp,
    week_start: jiff::Timestamp,
    until: jiff::Timestamp,
) -> Result<UsageGetResult, StoreError> {
    let mut accounts = Vec::new();
    for account_id in store.usage_account_ids()? {
        let today = usage_period(store.usage_summary(&account_id, day_start, until)?);
        let week = usage_period(store.usage_summary(&account_id, week_start, until)?);
        let limits = store
            .limit_snapshots(&account_id)?
            .into_iter()
            .map(limit_window)
            .collect();
        accounts.push(AccountUsage {
            account_id,
            today,
            week,
            limits,
        });
    }
    Ok(UsageGetResult { accounts })
}

fn usage_period(summary: wisp_store::UsageSummary) -> UsagePeriod {
    UsagePeriod {
        input_tokens: summary.input_tokens,
        output_tokens: summary.output_tokens,
        cache_read_tokens: summary.cache_read_tokens,
        cache_write_tokens: summary.cache_write_tokens,
        cost_usd_micros: summary.cost_usd_micros,
    }
}

fn limit_window(snapshot: LimitSnapshot) -> UsageLimitWindow {
    UsageLimitWindow {
        window: snapshot.window,
        used_percent: snapshot.used_percent,
        resets_at: snapshot.resets_at,
        captured_at: snapshot.captured_at,
    }
}

/// Local midnight today, and local midnight of the most recent Monday (today, if today is
/// Monday), for `now`.
fn local_bounds(now: &Zoned) -> Result<(jiff::Timestamp, jiff::Timestamp), jiff::Error> {
    let day_start = now.start_of_day()?;
    let monday_offset = i64::from(now.date().weekday().to_monday_zero_offset());
    let week_start_date = now.date().saturating_sub(monday_offset.days());
    let week_start = week_start_date
        .to_zoned(now.time_zone().clone())?
        .start_of_day()?;
    Ok((day_start.timestamp(), week_start.timestamp()))
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use uuid::Uuid;
    use wisp_store::{LimitSnapshot, Store, UsageDelta};

    use super::{local_bounds, usage_report};

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wisp.sqlite3");
        let store = Store::open(&path).unwrap();
        (dir, store)
    }

    #[test]
    fn week_start_is_the_most_recent_monday_in_the_zones_local_time() {
        for (day, expected_monday) in [
            (date(2026, 9, 21), date(2026, 9, 21)), // a Monday
            (date(2026, 9, 23), date(2026, 9, 21)), // mid-week
            (date(2026, 9, 27), date(2026, 9, 21)), // a Sunday: still last Monday
            (date(2026, 9, 28), date(2026, 9, 28)), // the next Monday
        ] {
            let zoned = day
                .at(15, 30, 0, 0)
                .to_zoned(jiff::tz::TimeZone::UTC)
                .unwrap();
            let (day_start, week_start) = local_bounds(&zoned).unwrap();
            assert_eq!(
                day_start,
                day.to_zoned(jiff::tz::TimeZone::UTC).unwrap().timestamp(),
                "{day:?}"
            );
            assert_eq!(
                week_start,
                expected_monday
                    .to_zoned(jiff::tz::TimeZone::UTC)
                    .unwrap()
                    .timestamp(),
                "{day:?}"
            );
        }
    }

    #[test]
    fn a_report_covers_every_account_with_usage_or_limits_and_omits_unreported_cost() {
        let (_dir, store) = open();
        store
            .record_usage_delta(&UsageDelta {
                run_id: Uuid::now_v7(),
                account_id: "claude-max".to_owned(),
                model: Some("opus".to_owned()),
                input_tokens: 100,
                output_tokens: 10,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                cost_usd_micros: Some(50),
                at: jiff::Timestamp::now(),
            })
            .unwrap();
        store
            .record_limit_snapshot(&LimitSnapshot {
                account_id: "codex-work".to_owned(),
                window: "primary".to_owned(),
                used_percent: Some(5.0),
                resets_at: None,
                captured_at: jiff::Timestamp::now(),
            })
            .unwrap();

        let far_past = "2020-01-01T00:00:00Z".parse().unwrap();
        let far_future = "2030-01-01T00:00:00Z".parse().unwrap();
        let report = usage_report(&store, far_past, far_past, far_future).unwrap();
        assert_eq!(report.accounts.len(), 2);

        let claude = report
            .accounts
            .iter()
            .find(|a| a.account_id == "claude-max")
            .unwrap();
        assert_eq!(claude.today.input_tokens, 100);
        assert_eq!(claude.today.cost_usd_micros, Some(50));
        assert!(claude.limits.is_empty());

        let codex = report
            .accounts
            .iter()
            .find(|a| a.account_id == "codex-work")
            .unwrap();
        assert_eq!(codex.today.input_tokens, 0);
        assert_eq!(
            codex.today.cost_usd_micros, None,
            "not reported, since codex-work has no usage deltas at all"
        );
        assert_eq!(codex.limits.len(), 1);
    }

    #[test]
    fn a_store_with_no_usage_reports_no_accounts() {
        let (_dir, store) = open();
        let far_past = "2020-01-01T00:00:00Z".parse().unwrap();
        let far_future = "2030-01-01T00:00:00Z".parse().unwrap();
        let report = usage_report(&store, far_past, far_past, far_future).unwrap();
        assert!(report.accounts.is_empty());
    }
}
