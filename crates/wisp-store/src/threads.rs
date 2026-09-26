use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use uuid::Uuid;

use crate::error::StoreError;
use crate::runs::insert_run;
use crate::worktree::insert_worktree;
use crate::{Run, RunFields, RunState, Store, Worktree, WorktreeFields, timestamp};

/// What registering a repo entry takes (#110).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoFields {
    pub name: String,
    /// The repository's canonical path. One entry per path.
    pub path: String,
    /// Whether this is wispd's scratch entry, for threads with no repo.
    pub scratch: bool,
}

/// A repo entry row: a repository normal threads run in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    pub id: Uuid,
    pub fields: RepoFields,
    pub created_at: Timestamp,
}

/// A normal thread row, keyed by its run's id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub archived: bool,
    pub created_at: Timestamp,
}

const REPO_COLUMNS: &str = "id, name, path, scratch, created_at";
const THREAD_COLUMNS: &str = "id, repo_id, archived, created_at";

fn repo_from_row(row: &Row<'_>) -> rusqlite::Result<(String, String, String, bool, String)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn into_repo(raw: (String, String, String, bool, String)) -> Result<Repo, StoreError> {
    let (id, name, path, scratch, created_at) = raw;
    Ok(Repo {
        id: Uuid::parse_str(&id)?,
        fields: RepoFields {
            name,
            path,
            scratch,
        },
        created_at: timestamp::parse(&created_at)?,
    })
}

fn thread_from_row(row: &Row<'_>) -> rusqlite::Result<(String, String, bool, String)> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn into_thread(raw: (String, String, bool, String)) -> Result<Thread, StoreError> {
    let (id, repo_id, archived, created_at) = raw;
    Ok(Thread {
        id: Uuid::parse_str(&id)?,
        repo_id: Uuid::parse_str(&repo_id)?,
        archived,
        created_at: timestamp::parse(&created_at)?,
    })
}

fn fetch_repo(conn: &Connection, sql_where: &str, key: &str) -> Result<Option<Repo>, StoreError> {
    conn.query_row(
        &format!("SELECT {REPO_COLUMNS} FROM repos WHERE {sql_where}"),
        params![key],
        repo_from_row,
    )
    .optional()?
    .map(into_repo)
    .transpose()
}

fn fetch_thread(conn: &Connection, id: Uuid) -> Result<Option<Thread>, StoreError> {
    conn.query_row(
        &format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id = ?1"),
        params![id.to_string()],
        thread_from_row,
    )
    .optional()?
    .map(into_thread)
    .transpose()
}

impl Store {
    /// Registers a repo entry `id` for `fields.path`, or returns the entry that already has that
    /// path, whatever its id.
    ///
    /// # Errors
    ///
    /// [`StoreError::IdConflict`] if `id` is taken by an entry with another path, or a database
    /// error.
    pub fn add_repo(&mut self, id: Uuid, fields: &RepoFields) -> Result<Repo, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = fetch_repo(&tx, "path = ?1", &fields.path)? {
            return Ok(existing);
        }
        if fetch_repo(&tx, "id = ?1", &id.to_string())?.is_some() {
            return Err(StoreError::IdConflict { id });
        }
        tx.execute(
            "INSERT INTO repos (id, name, path, scratch, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                id.to_string(),
                fields.name,
                fields.path,
                fields.scratch,
                timestamp::now()
            ],
        )?;
        let repo =
            fetch_repo(&tx, "id = ?1", &id.to_string())?.ok_or(StoreError::NotFound { id })?;
        tx.commit()?;
        Ok(repo)
    }

    /// Reads a repo entry.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or timestamp is corrupt.
    pub fn get_repo(&self, id: Uuid) -> Result<Option<Repo>, StoreError> {
        fetch_repo(&self.conn, "id = ?1", &id.to_string())
    }

    /// wispd's scratch entry, once it made one.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or timestamp is corrupt.
    pub fn scratch_repo(&self) -> Result<Option<Repo>, StoreError> {
        self.conn
            .query_row(
                &format!(
                    "SELECT {REPO_COLUMNS} FROM repos WHERE scratch = 1 \
                     ORDER BY created_at ASC, id ASC LIMIT 1"
                ),
                [],
                repo_from_row,
            )
            .optional()?
            .map(into_repo)
            .transpose()
    }

    /// Every repo entry, oldest first.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or timestamp is corrupt.
    pub fn list_repos(&self) -> Result<Vec<Repo>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {REPO_COLUMNS} FROM repos ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map([], repo_from_row)?;
        let mut repos = Vec::new();
        for row in rows {
            repos.push(into_repo(row?)?);
        }
        Ok(repos)
    }

    /// Records a normal thread in repo entry `repo_id`, with its run and the run's worktree, in
    /// one transaction, so none exists without the others. The run's `project_id` must be
    /// `repo_id`.
    ///
    /// # Errors
    ///
    /// [`StoreError::IdConflict`] if a run, a worktree, or a thread with `id` exists, in which
    /// case nothing is written, or a database error.
    pub fn create_thread_run(
        &mut self,
        id: Uuid,
        repo_id: Uuid,
        fields: &RunFields,
        state: &RunState,
        worktree: &WorktreeFields,
    ) -> Result<(Thread, Run, Worktree), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let worktree = insert_worktree(&tx, id, worktree)?;
        let run = insert_run(&tx, id, fields, state)?;
        let inserted = tx.execute(
            "INSERT INTO threads (id, repo_id, archived, created_at) VALUES (?1, ?2, 0, ?3)
             ON CONFLICT (id) DO NOTHING",
            params![id.to_string(), repo_id.to_string(), timestamp::now()],
        )?;
        if inserted == 0 {
            return Err(StoreError::IdConflict { id });
        }
        let thread = fetch_thread(&tx, id)?.ok_or(StoreError::NotFound { id })?;
        tx.commit()?;
        Ok((thread, run, worktree))
    }

    /// Reads a thread.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or timestamp is corrupt.
    pub fn get_thread(&self, id: Uuid) -> Result<Option<Thread>, StoreError> {
        fetch_thread(&self.conn, id)
    }

    /// Every thread, oldest first.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or timestamp is corrupt.
    pub fn list_threads(&self) -> Result<Vec<Thread>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {THREAD_COLUMNS} FROM threads ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map([], thread_from_row)?;
        let mut threads = Vec::new();
        for row in rows {
            threads.push(into_thread(row?)?);
        }
        Ok(threads)
    }

    /// Archives thread `id` or brings it back, and returns it.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if no thread has `id`, or a database error.
    pub fn set_thread_archived(&self, id: Uuid, archived: bool) -> Result<Thread, StoreError> {
        let changed = self.conn.execute(
            "UPDATE threads SET archived = ?2 WHERE id = ?1",
            params![id.to_string(), archived],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound { id });
        }
        fetch_thread(&self.conn, id)?.ok_or(StoreError::NotFound { id })
    }

    /// Deletes thread `id` with its run, its worktree row, its stored events, and its sent turns
    /// (#190: `turns` has no foreign key to `runs`, so nothing else removes them), in one
    /// transaction. Returns whether the thread existed.
    ///
    /// # Errors
    ///
    /// A database error.
    pub fn delete_thread(&mut self, id: Uuid) -> Result<bool, StoreError> {
        let key = id.to_string();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existed = tx.execute("DELETE FROM threads WHERE id = ?1", params![key])? > 0;
        tx.execute("DELETE FROM runs WHERE id = ?1", params![key])?;
        tx.execute("DELETE FROM worktrees WHERE id = ?1", params![key])?;
        tx.execute("DELETE FROM events WHERE run_id = ?1", params![key])?;
        tx.execute("DELETE FROM turns WHERE run_id = ?1", params![key])?;
        tx.commit()?;
        Ok(existed)
    }
}
