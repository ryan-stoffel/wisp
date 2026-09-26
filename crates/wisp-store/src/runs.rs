use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use uuid::Uuid;

use crate::error::StoreError;
use crate::worktree::insert_worktree;
use crate::{Store, Worktree, WorktreeFields, timestamp};

/// What an `agent/start` asked for, plus the backend routing resolved it to (#156). None of it
/// changes after the run is created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunFields {
    pub project_id: Uuid,
    pub prompt: String,
    /// The JSON of the account the request named, or `None` for the role's default.
    pub requested_account: Option<String>,
    pub policy: String,
    pub backend: String,
}

/// A run's state, which changes as it runs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunState {
    pub status: String,
    /// The account usage is charged to now: the one routing resolved, until a fallback.
    pub account_id: String,
    /// The vendor's session id, once the CLI reported it. A later run resumes it.
    pub session_id: Option<String>,
    /// Why the run failed, for people.
    pub error: Option<String>,
    /// The last commit wispd made for the run, and its stats against the worktree's base.
    pub commit_sha: Option<String>,
    pub files_changed: Option<u64>,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
    /// How `agent/accept` merged the run, once it did (#157).
    pub accept: Option<RunAccept>,
}

/// What `agent/accept` did for a run (#157).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunAccept {
    /// The accept's client-generated id, for its idempotency.
    pub id: Uuid,
    /// The commit the project's branch points at afterward.
    pub commit: String,
    /// The branch it went into.
    pub into: String,
    /// `fastForward`, `merge`, or `upToDate`.
    pub how: String,
}

/// A run row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: Uuid,
    pub fields: RunFields,
    pub state: RunState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

const COLUMNS: &str = "id, project_id, prompt, requested_account, policy, backend, account_id, \
                       status, session_id, error, commit_sha, files_changed, insertions, \
                       deletions, created_at, updated_at, accept_id, merge_commit, \
                       merge_into, merge_how";

struct RawRun {
    id: String,
    project_id: String,
    prompt: String,
    requested_account: Option<String>,
    policy: String,
    backend: String,
    account_id: String,
    status: String,
    session_id: Option<String>,
    error: Option<String>,
    commit_sha: Option<String>,
    files_changed: Option<u64>,
    insertions: Option<u64>,
    deletions: Option<u64>,
    created_at: String,
    updated_at: String,
    accept_id: Option<String>,
    merge_commit: Option<String>,
    merge_into: Option<String>,
    merge_how: Option<String>,
}

impl RawRun {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            project_id: row.get(1)?,
            prompt: row.get(2)?,
            requested_account: row.get(3)?,
            policy: row.get(4)?,
            backend: row.get(5)?,
            account_id: row.get(6)?,
            status: row.get(7)?,
            session_id: row.get(8)?,
            error: row.get(9)?,
            commit_sha: row.get(10)?,
            files_changed: row.get(11)?,
            insertions: row.get(12)?,
            deletions: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
            accept_id: row.get(16)?,
            merge_commit: row.get(17)?,
            merge_into: row.get(18)?,
            merge_how: row.get(19)?,
        })
    }

    fn into_run(self) -> Result<Run, StoreError> {
        let accept = match (
            self.accept_id,
            self.merge_commit,
            self.merge_into,
            self.merge_how,
        ) {
            (Some(id), Some(commit), Some(into), Some(how)) => Some(RunAccept {
                id: Uuid::parse_str(&id)?,
                commit,
                into,
                how,
            }),
            _ => None,
        };
        Ok(Run {
            id: Uuid::parse_str(&self.id)?,
            fields: RunFields {
                project_id: Uuid::parse_str(&self.project_id)?,
                prompt: self.prompt,
                requested_account: self.requested_account,
                policy: self.policy,
                backend: self.backend,
            },
            state: RunState {
                status: self.status,
                account_id: self.account_id,
                session_id: self.session_id,
                error: self.error,
                commit_sha: self.commit_sha,
                files_changed: self.files_changed,
                insertions: self.insertions,
                deletions: self.deletions,
                accept,
            },
            created_at: timestamp::parse(&self.created_at)?,
            updated_at: timestamp::parse(&self.updated_at)?,
        })
    }
}

fn fetch(conn: &Connection, id: Uuid) -> Result<Option<Run>, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM runs WHERE id = ?1"),
        params![id.to_string()],
        RawRun::from_row,
    )
    .optional()?
    .map(RawRun::into_run)
    .transpose()
}

impl Store {
    /// Records a new run `id` in its first `state`.
    ///
    /// A run is created once: the caller looks it up first and answers a retry of `agent/start`
    /// from the existing row, so this never replaces one.
    ///
    /// # Errors
    ///
    /// [`StoreError::IdConflict`] if a run with `id` exists, or a database error.
    pub fn create_run(
        &self,
        id: Uuid,
        fields: &RunFields,
        state: &RunState,
    ) -> Result<Run, StoreError> {
        insert_run(&self.conn, id, fields, state)
    }

    /// Records a new run `id` and its worktree together, in one transaction, so neither exists
    /// without the other.
    ///
    /// # Errors
    ///
    /// [`StoreError::IdConflict`] if a run or a worktree with `id` exists, in which case
    /// nothing is written, or a database error.
    pub fn create_run_with_worktree(
        &mut self,
        id: Uuid,
        fields: &RunFields,
        state: &RunState,
        worktree: &WorktreeFields,
    ) -> Result<(Run, Worktree), StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let worktree = insert_worktree(&tx, id, worktree)?;
        let run = insert_run(&tx, id, fields, state)?;
        tx.commit()?;
        Ok((run, worktree))
    }

    /// Reads a run.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or timestamp is corrupt.
    pub fn get_run(&self, id: Uuid) -> Result<Option<Run>, StoreError> {
        fetch(&self.conn, id)
    }

    /// Every run, or `project`'s, oldest first.
    ///
    /// # Errors
    ///
    /// A database error, or an error if a stored id or timestamp is corrupt.
    pub fn list_runs(&self, project: Option<Uuid>) -> Result<Vec<Run>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM runs
             WHERE ?1 IS NULL OR project_id = ?1
             ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map(params![project.map(|id| id.to_string())], RawRun::from_row)?;
        let mut runs = Vec::new();
        for row in rows {
            runs.push(row?.into_run()?);
        }
        Ok(runs)
    }

    /// Replaces run `id`'s state and returns the updated row.
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if no run has `id`, or a database error.
    pub fn update_run(&self, id: Uuid, state: &RunState) -> Result<Run, StoreError> {
        update(&self.conn, id, state)
    }

    /// Replaces accepted run `id`'s state and deletes its worktree's row, in one transaction:
    /// `agent/accept` removed the worktree and its branch (#157).
    ///
    /// # Errors
    ///
    /// [`StoreError::NotFound`] if no run has `id`, in which case nothing is written, or a
    /// database error.
    pub fn accept_run(&mut self, id: Uuid, state: &RunState) -> Result<Run, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = update(&tx, id, state)?;
        tx.execute(
            "DELETE FROM worktrees WHERE id = ?1",
            params![id.to_string()],
        )?;
        tx.commit()?;
        Ok(run)
    }
}

fn update(conn: &Connection, id: Uuid, state: &RunState) -> Result<Run, StoreError> {
    let accept = state.accept.as_ref();
    let changed = conn.execute(
        "UPDATE runs SET status = ?2, account_id = ?3, session_id = ?4, error = ?5,
                         commit_sha = ?6, files_changed = ?7, insertions = ?8,
                         deletions = ?9, updated_at = ?10, accept_id = ?11,
                         merge_commit = ?12, merge_into = ?13, merge_how = ?14
         WHERE id = ?1",
        params![
            id.to_string(),
            state.status,
            state.account_id,
            state.session_id,
            state.error,
            state.commit_sha,
            state.files_changed,
            state.insertions,
            state.deletions,
            timestamp::now(),
            accept.map(|accept| accept.id.to_string()),
            accept.map(|accept| accept.commit.as_str()),
            accept.map(|accept| accept.into.as_str()),
            accept.map(|accept| accept.how.as_str()),
        ],
    )?;
    if changed == 0 {
        return Err(StoreError::NotFound { id });
    }
    fetch(conn, id)?.ok_or(StoreError::NotFound { id })
}

pub(crate) fn insert_run(
    conn: &Connection,
    id: Uuid,
    fields: &RunFields,
    state: &RunState,
) -> Result<Run, StoreError> {
    let now = timestamp::now();
    let inserted = conn.execute(
        "INSERT INTO runs (id, project_id, prompt, requested_account, policy, backend,
                           account_id, status, session_id, error, commit_sha,
                           files_changed, insertions, deletions, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?15)
         ON CONFLICT (id) DO NOTHING",
        params![
            id.to_string(),
            fields.project_id.to_string(),
            fields.prompt,
            fields.requested_account,
            fields.policy,
            fields.backend,
            state.account_id,
            state.status,
            state.session_id,
            state.error,
            state.commit_sha,
            state.files_changed,
            state.insertions,
            state.deletions,
            now,
        ],
    )?;
    if inserted == 0 {
        return Err(StoreError::IdConflict { id });
    }
    fetch(conn, id)?.ok_or(StoreError::NotFound { id })
}
