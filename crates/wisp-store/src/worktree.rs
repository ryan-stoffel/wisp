use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use uuid::Uuid;

use crate::Store;
use crate::error::StoreError;
use crate::timestamp;

/// The fields of a worktree that a caller supplies. `id` is the run it was created for: a
/// worktree is created for exactly one run (#154).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeFields {
    pub repo_path: String,
    pub path: String,
    pub branch: String,
    pub base: String,
}

/// A worktree row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub id: Uuid,
    pub repo_path: String,
    pub path: String,
    pub branch: String,
    pub base: String,
    pub created_at: Timestamp,
}

/// A worktree row with its id and timestamp still as the TEXT SQLite stored them, before the
/// fallible conversion to [`Worktree`].
struct RawWorktree {
    id: String,
    repo_path: String,
    path: String,
    branch: String,
    base: String,
    created_at: String,
}

impl RawWorktree {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            repo_path: row.get(1)?,
            path: row.get(2)?,
            branch: row.get(3)?,
            base: row.get(4)?,
            created_at: row.get(5)?,
        })
    }

    fn matches(&self, fields: &WorktreeFields) -> bool {
        self.repo_path == fields.repo_path
            && self.path == fields.path
            && self.branch == fields.branch
            && self.base == fields.base
    }

    fn into_worktree(self) -> Result<Worktree, StoreError> {
        Ok(Worktree {
            id: Uuid::parse_str(&self.id)?,
            repo_path: self.repo_path,
            path: self.path,
            branch: self.branch,
            base: self.base,
            created_at: timestamp::parse(&self.created_at)?,
        })
    }
}

fn fetch_raw(conn: &Connection, id_text: &str) -> Result<Option<RawWorktree>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT id, repo_path, path, branch, base, created_at
             FROM worktrees WHERE id = ?1",
            params![id_text],
            RawWorktree::from_row,
        )
        .optional()?)
}

impl Store {
    /// Records the worktree created for run `id`.
    ///
    /// If a worktree for `id` already exists with the same `fields`, this returns that existing
    /// row unchanged instead of creating a second one. If it exists with different fields, it
    /// fails with [`StoreError::IdConflict`] rather than overwrite it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::IdConflict`] as described above, or a database error.
    pub fn create_worktree(
        &mut self,
        id: Uuid,
        fields: &WorktreeFields,
    ) -> Result<Worktree, StoreError> {
        let id_text = id.to_string();

        // See `create_project`'s fast path: a retry that already matches doesn't need the write
        // lock.
        if let Some(existing) = fetch_raw(&self.conn, &id_text)?
            && existing.matches(fields)
        {
            return existing.into_worktree();
        }

        let now = timestamp::now();

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO worktrees (id, repo_path, path, branch, base, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (id) DO NOTHING",
            params![
                id_text,
                fields.repo_path,
                fields.path,
                fields.branch,
                fields.base,
                now
            ],
        )?;
        let created = tx.changes() == 1;

        let raw = fetch_raw(&tx, &id_text)?.ok_or(StoreError::NotFound { id })?;

        if !created && !raw.matches(fields) {
            return Err(StoreError::IdConflict { id });
        }

        tx.commit()?;
        raw.into_worktree()
    }

    /// Reads a worktree by its run id.
    ///
    /// # Errors
    ///
    /// Returns a database error, or an error if the stored id or timestamp is corrupt.
    pub fn get_worktree(&self, id: Uuid) -> Result<Option<Worktree>, StoreError> {
        fetch_raw(&self.conn, &id.to_string())?
            .map(RawWorktree::into_worktree)
            .transpose()
    }

    /// Lists every worktree, oldest first.
    ///
    /// Startup garbage collection uses this to tell which worktree folders under the wisp-owned
    /// root are still recorded, and treats anything else there as an orphan.
    ///
    /// # Errors
    ///
    /// Returns a database error, or an error if a stored id or timestamp is corrupt.
    pub fn list_worktrees(&self) -> Result<Vec<Worktree>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, repo_path, path, branch, base, created_at
             FROM worktrees
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], RawWorktree::from_row)?;

        let mut worktrees = Vec::new();
        for row in rows {
            worktrees.push(row?.into_worktree()?);
        }
        Ok(worktrees)
    }

    /// Deletes a worktree's row by its run id, if it exists.
    ///
    /// Returns whether a row was deleted.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn delete_worktree(&self, id: Uuid) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "DELETE FROM worktrees WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(changed > 0)
    }
}
