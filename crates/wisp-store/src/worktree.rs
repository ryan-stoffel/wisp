use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, params};
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
    /// The linked worktree's own private git directory, resolved once when it was created
    /// (#166). Every later git call pins it instead of trusting the worktree's `.git` file, which
    /// a worker can rewrite, so it is stored rather than derived again after a restart.
    pub git_dir: String,
}

/// A worktree row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub id: Uuid,
    pub repo_path: String,
    pub path: String,
    pub branch: String,
    pub base: String,
    /// See [`WorktreeFields::git_dir`]. Empty for a row written before the column existed.
    pub git_dir: String,
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
    git_dir: String,
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
            git_dir: row.get(5)?,
            created_at: row.get(6)?,
        })
    }

    fn into_worktree(self) -> Result<Worktree, StoreError> {
        Ok(Worktree {
            id: Uuid::parse_str(&self.id)?,
            repo_path: self.repo_path,
            path: self.path,
            branch: self.branch,
            base: self.base,
            git_dir: self.git_dir,
            created_at: timestamp::parse(&self.created_at)?,
        })
    }
}

fn fetch_raw(conn: &Connection, id_text: &str) -> Result<Option<RawWorktree>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT id, repo_path, path, branch, base, git_dir, created_at
             FROM worktrees WHERE id = ?1",
            params![id_text],
            RawWorktree::from_row,
        )
        .optional()?)
}

impl Store {
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
}

/// Inserts a new worktree row for run `id`, failing with [`StoreError::IdConflict`] if one
/// exists.
pub(crate) fn insert_worktree(
    conn: &Connection,
    id: Uuid,
    fields: &WorktreeFields,
) -> Result<Worktree, StoreError> {
    let id_text = id.to_string();
    let inserted = conn.execute(
        "INSERT INTO worktrees (id, repo_path, path, branch, base, git_dir, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (id) DO NOTHING",
        params![
            id_text,
            fields.repo_path,
            fields.path,
            fields.branch,
            fields.base,
            fields.git_dir,
            timestamp::now()
        ],
    )?;
    if inserted == 0 {
        return Err(StoreError::IdConflict { id });
    }
    fetch_raw(conn, &id_text)?
        .ok_or(StoreError::NotFound { id })?
        .into_worktree()
}
