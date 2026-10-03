use jiff::Timestamp;
use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use crate::Store;
use crate::error::StoreError;
use crate::timestamp;

/// Records that `run_id`'s `turn_id` was sent with `text`, so `agent/send` stays idempotent on
/// `turnId` across a plxd restart (#190): a fresh `Actor` after a restart has no in-memory record
/// of what it already sent to a run's CLI. Does nothing if this `(run_id, turn_id)` is already
/// recorded, which is what makes a retry idempotent rather than an error.
impl Store {
    /// # Errors
    ///
    /// A database error.
    pub fn record_turn(&self, run_id: Uuid, turn_id: Uuid, text: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO turns (run_id, turn_id, text, created_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (run_id, turn_id) DO NOTHING",
            params![
                run_id.to_string(),
                turn_id.to_string(),
                text,
                timestamp::now()
            ],
        )?;
        Ok(())
    }

    /// Every turn recorded for `run_id`, in no particular order: what a restarted actor rebuilds
    /// its idempotency map from.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id is corrupt.
    pub fn run_turns(&self, run_id: Uuid) -> Result<Vec<(Uuid, String)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT turn_id, text FROM turns WHERE run_id = ?1")?;
        let rows = stmt.query_map(params![run_id.to_string()], row_to_turn)?;
        let mut turns = Vec::new();
        for row in rows {
            let (turn_id, text) = row?;
            turns.push((Uuid::parse_str(&turn_id)?, text));
        }
        Ok(turns)
    }

    /// The id of `run_id`'s newest recorded turn, or `None` if it has none: where a fork with no
    /// turn named continues from (0050).
    ///
    /// # Errors
    ///
    /// A database error, or an error if the stored id is corrupt.
    pub fn latest_turn(&self, run_id: Uuid) -> Result<Option<Uuid>, StoreError> {
        let turn: Option<String> = self
            .conn
            .query_row(
                "SELECT turn_id FROM turns WHERE run_id = ?1
                 ORDER BY created_at DESC, rowid DESC LIMIT 1",
                params![run_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(turn.as_deref().map(Uuid::parse_str).transpose()?)
    }

    /// When `run_id`'s newest recorded turn was sent, or `None` if it has none: a coordinator's
    /// last turn, which wake-ups rebuilt after a restart count from (RYA-178).
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored timestamp is corrupt.
    pub fn last_turn_at(&self, run_id: Uuid) -> Result<Option<Timestamp>, StoreError> {
        let at: Option<String> = self.conn.query_row(
            "SELECT MAX(created_at) FROM turns WHERE run_id = ?1",
            params![run_id.to_string()],
            |row| row.get(0),
        )?;
        at.as_deref().map(timestamp::parse).transpose()
    }
}

fn row_to_turn(row: &Row<'_>) -> rusqlite::Result<(String, String)> {
    Ok((row.get(0)?, row.get(1)?))
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::Store;

    #[test]
    fn recording_a_turn_twice_is_idempotent_and_turns_are_per_run() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("parallax.sqlite3")).unwrap();
        let (run, other_run) = (Uuid::now_v7(), Uuid::now_v7());
        let turn = Uuid::now_v7();

        store.record_turn(run, turn, "do the thing").unwrap();
        store.record_turn(run, turn, "do the thing").unwrap();
        assert_eq!(
            store.run_turns(run).unwrap(),
            [(turn, "do the thing".to_owned())]
        );
        assert_eq!(store.run_turns(other_run).unwrap(), []);
        assert!(store.last_turn_at(run).unwrap().is_some());
        let later = Uuid::now_v7();
        store.record_turn(run, later, "and then this").unwrap();
        assert_eq!(store.latest_turn(run).unwrap(), Some(later));
        assert_eq!(store.latest_turn(other_run).unwrap(), None);
        assert_eq!(store.last_turn_at(other_run).unwrap(), None);
    }
}
