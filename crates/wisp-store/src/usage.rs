//! Per-account usage: token and cost deltas, limit-window snapshots, and per-session running
//! totals so a resumed session can report only what it adds (#113's `Resume::usage_totals`).
//!
//! `model` columns use `''` as the "no model reported" sentinel rather than `NULL`, because
//! SQLite's `PRIMARY KEY` treats two `NULL`s as distinct values, which would stop
//! `session_usage_totals` from replacing an existing row for the unnamed model.

use jiff::Timestamp;
use rusqlite::{TransactionBehavior, params};
use uuid::Uuid;

use crate::Store;
use crate::error::StoreError;
use crate::timestamp;

fn model_key(model: Option<&str>) -> &str {
    model.unwrap_or("")
}

/// One run's token and cost delta for one model, ready to append.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageDelta {
    pub run_id: Uuid,
    pub account_id: String,
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// The cost the vendor reported, when it reports one (0004: Claude estimates one, Codex and
    /// Cursor report none).
    pub cost_usd_micros: Option<u64>,
    /// When wispd recorded the delta.
    pub at: Timestamp,
}

/// One account's summed usage over a half-open time range `[since, until)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UsageSummary {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// `None` when no delta in the range reported a cost — either there was no usage, or the
    /// vendor never reports one (0004: Codex, Cursor) — as opposed to a reported cost of zero.
    pub cost_usd_micros: Option<u64>,
}

/// One of an account's limit windows, as a vendor last reported it.
#[derive(Debug, Clone, PartialEq)]
pub struct LimitSnapshot {
    pub account_id: String,
    /// The vendor's name for the window, such as `five_hour` or `primary`.
    pub window: String,
    pub used_percent: Option<f64>,
    pub resets_at: Option<Timestamp>,
    /// When wispd captured this snapshot.
    pub captured_at: Timestamp,
}

/// One model's running usage total for a session, ready to become a resumed run's
/// `Resume::usage_totals` baseline.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionModelUsage {
    pub model: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd_micros: Option<u64>,
}

struct RawLimitSnapshot {
    account_id: String,
    window: String,
    used_percent: Option<f64>,
    resets_at: Option<String>,
    captured_at: String,
}

impl RawLimitSnapshot {
    fn into_snapshot(self) -> Result<LimitSnapshot, StoreError> {
        Ok(LimitSnapshot {
            account_id: self.account_id,
            window: self.window,
            used_percent: self.used_percent,
            resets_at: self
                .resets_at
                .map(|text| timestamp::parse(&text))
                .transpose()?,
            captured_at: timestamp::parse(&self.captured_at)?,
        })
    }
}

impl Store {
    /// Appends one usage delta. Deltas are never merged or updated; a reader sums them with
    /// [`Store::usage_summary`].
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn record_usage_delta(&self, delta: &UsageDelta) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO usage_deltas
                (run_id, account_id, model, input_tokens, output_tokens, cache_read_tokens,
                 cache_write_tokens, cost_usd_micros, at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                delta.run_id.to_string(),
                delta.account_id,
                model_key(delta.model.as_deref()),
                delta.input_tokens,
                delta.output_tokens,
                delta.cache_read_tokens,
                delta.cache_write_tokens,
                delta.cost_usd_micros,
                timestamp::format(delta.at),
            ],
        )?;
        Ok(())
    }

    /// `account_id`'s summed usage in `[since, until)`. Safe for an account with no rows: every
    /// count is zero and the cost is `None`.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn usage_summary(
        &self,
        account_id: &str,
        since: Timestamp,
        until: Timestamp,
    ) -> Result<UsageSummary, StoreError> {
        self.conn
            .query_row(
                "SELECT
                    COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0),
                    COALESCE(SUM(cache_read_tokens), 0),
                    COALESCE(SUM(cache_write_tokens), 0),
                    SUM(cost_usd_micros),
                    COUNT(cost_usd_micros)
                 FROM usage_deltas
                 WHERE account_id = ?1 AND at >= ?2 AND at < ?3",
                params![
                    account_id,
                    timestamp::format(since),
                    timestamp::format(until)
                ],
                |row| {
                    let reported: i64 = row.get(5)?;
                    let cost_usd_micros = if reported > 0 {
                        Some(row.get::<_, u64>(4)?)
                    } else {
                        None
                    };
                    Ok(UsageSummary {
                        input_tokens: row.get(0)?,
                        output_tokens: row.get(1)?,
                        cache_read_tokens: row.get(2)?,
                        cache_write_tokens: row.get(3)?,
                        cost_usd_micros,
                    })
                },
            )
            .map_err(Into::into)
    }

    /// Every account id with a recorded usage delta or limit snapshot, ascending.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn usage_account_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id FROM usage_deltas
             UNION
             SELECT account_id FROM limit_snapshots
             ORDER BY account_id ASC",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Replaces the account's snapshot for `snapshot.window`, unless the snapshot already there
    /// was captured more recently — guards against an event that arrives out of order.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn record_limit_snapshot(&self, snapshot: &LimitSnapshot) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO limit_snapshots (account_id, window, used_percent, resets_at, captured_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (account_id, window) DO UPDATE SET
                used_percent = excluded.used_percent,
                resets_at = excluded.resets_at,
                captured_at = excluded.captured_at
             WHERE excluded.captured_at >= limit_snapshots.captured_at",
            params![
                snapshot.account_id,
                snapshot.window,
                snapshot.used_percent,
                snapshot.resets_at.map(timestamp::format),
                timestamp::format(snapshot.captured_at),
            ],
        )?;
        Ok(())
    }

    /// Every limit window last reported for `account_id`, ordered by window name. Empty for an
    /// account with none reported.
    ///
    /// # Errors
    ///
    /// Returns a database error, or an error if a stored timestamp is corrupt.
    pub fn limit_snapshots(&self, account_id: &str) -> Result<Vec<LimitSnapshot>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, window, used_percent, resets_at, captured_at
             FROM limit_snapshots WHERE account_id = ?1 ORDER BY window ASC",
        )?;
        let rows = stmt.query_map(params![account_id], |row| {
            Ok(RawLimitSnapshot {
                account_id: row.get(0)?,
                window: row.get(1)?,
                used_percent: row.get(2)?,
                resets_at: row.get(3)?,
                captured_at: row.get(4)?,
            })
        })?;
        rows.map(|row| row?.into_snapshot()).collect()
    }

    /// Replaces `session_id`'s running usage totals wholesale with `totals`, so they match the
    /// vendor's current totals exactly instead of accumulating on top of a stale baseline (0004:
    /// Claude and Codex report cumulative totals that carry into a resumed session).
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn set_session_usage_totals(
        &mut self,
        session_id: &str,
        totals: &[SessionModelUsage],
    ) -> Result<(), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "DELETE FROM session_usage_totals WHERE session_id = ?1",
            params![session_id],
        )?;
        for total in totals {
            tx.execute(
                "INSERT INTO session_usage_totals
                    (session_id, model, input_tokens, output_tokens, cache_read_tokens,
                     cache_write_tokens, cost_usd_micros)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    session_id,
                    model_key(total.model.as_deref()),
                    total.input_tokens,
                    total.output_tokens,
                    total.cache_read_tokens,
                    total.cache_write_tokens,
                    total.cost_usd_micros,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// `session_id`'s running totals per model, for a `Resume::usage_totals` baseline. Empty for
    /// an unknown or fresh session.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn session_usage_totals(
        &self,
        session_id: &str,
    ) -> Result<Vec<SessionModelUsage>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT model, input_tokens, output_tokens, cache_read_tokens, cache_write_tokens,
                    cost_usd_micros
             FROM session_usage_totals WHERE session_id = ?1 ORDER BY model ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            let model: String = row.get(0)?;
            Ok(SessionModelUsage {
                model: Some(model).filter(|model| !model.is_empty()),
                input_tokens: row.get(1)?,
                output_tokens: row.get(2)?,
                cache_read_tokens: row.get(3)?,
                cache_write_tokens: row.get(4)?,
                cost_usd_micros: row.get(5)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }
}
