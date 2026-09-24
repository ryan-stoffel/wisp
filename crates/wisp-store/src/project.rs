use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::Store;
use crate::error::StoreError;
use crate::timestamp;

/// The fields of a project that a caller supplies and can change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFields {
    pub name: String,
    pub repo_path: String,
    pub host: String,
}

/// A project row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub repo_path: String,
    pub host: String,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A project row with its id and timestamps still as the TEXT SQLite stored
/// them, before the fallible conversion to [`Project`].
struct RawProject {
    id: String,
    name: String,
    repo_path: String,
    host: String,
    created_at: String,
    updated_at: String,
}

impl RawProject {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            repo_path: row.get(2)?,
            host: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    }

    fn matches(&self, fields: &ProjectFields) -> bool {
        self.name == fields.name && self.repo_path == fields.repo_path && self.host == fields.host
    }

    fn into_project(self) -> Result<Project, StoreError> {
        Ok(Project {
            id: Uuid::parse_str(&self.id)?,
            name: self.name,
            repo_path: self.repo_path,
            host: self.host,
            created_at: timestamp::parse(&self.created_at)?,
            updated_at: timestamp::parse(&self.updated_at)?,
        })
    }
}

fn fetch_raw(conn: &Connection, id_text: &str) -> Result<Option<RawProject>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT id, name, repo_path, host, created_at, updated_at
             FROM projects WHERE id = ?1",
            params![id_text],
            RawProject::from_row,
        )
        .optional()?)
}

impl Store {
    /// Creates a project owned by `id`.
    ///
    /// If a project with `id` already exists with the same `fields`, this
    /// returns that existing row unchanged instead of creating a second one.
    /// If it exists with different fields, it fails with
    /// [`StoreError::IdConflict`] rather than overwrite it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::IdConflict`] as described above, or a database
    /// error.
    pub fn create_project(
        &mut self,
        id: Uuid,
        fields: &ProjectFields,
    ) -> Result<Project, StoreError> {
        let id_text = id.to_string();
        let now = timestamp::now()?;

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO projects (id, name, repo_path, host, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT (id) DO NOTHING",
            params![id_text, fields.name, fields.repo_path, fields.host, now],
        )?;
        let created = tx.changes() == 1;

        // The row must exist now: we either just inserted it, or the
        // INSERT was ignored because it already existed.
        let raw = fetch_raw(&tx, &id_text)?.ok_or(StoreError::NotFound { id })?;

        if !created && !raw.matches(fields) {
            return Err(StoreError::IdConflict { id });
        }

        tx.commit()?;
        raw.into_project()
    }

    /// Reads a project by id.
    ///
    /// # Errors
    ///
    /// Returns a database error, or an error if the stored id or timestamps
    /// are corrupt.
    pub fn get_project(&self, id: Uuid) -> Result<Option<Project>, StoreError> {
        fetch_raw(&self.conn, &id.to_string())?
            .map(RawProject::into_project)
            .transpose()
    }

    /// Lists every project, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a database error, or an error if a stored id or timestamps
    /// are corrupt.
    pub fn list_projects(&self) -> Result<Vec<Project>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, repo_path, host, created_at, updated_at
             FROM projects
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], RawProject::from_row)?;

        let mut projects = Vec::new();
        for row in rows {
            projects.push(row?.into_project()?);
        }
        Ok(projects)
    }

    /// Replaces the mutable fields of an existing project and bumps
    /// `updated_at`.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] if no project has `id`, or a
    /// database error.
    pub fn update_project(&self, id: Uuid, fields: &ProjectFields) -> Result<Project, StoreError> {
        let id_text = id.to_string();
        let now = timestamp::now()?;

        let changed = self.conn.execute(
            "UPDATE projects
             SET name = ?2, repo_path = ?3, host = ?4, updated_at = ?5
             WHERE id = ?1",
            params![id_text, fields.name, fields.repo_path, fields.host, now],
        )?;
        if changed == 0 {
            return Err(StoreError::NotFound { id });
        }

        fetch_raw(&self.conn, &id_text)?
            .ok_or(StoreError::NotFound { id })?
            .into_project()
    }

    /// Deletes a project by id, if it exists.
    ///
    /// Returns whether a row was deleted.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn delete_project(&self, id: Uuid) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "DELETE FROM projects WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(changed > 0)
    }
}
