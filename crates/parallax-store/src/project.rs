use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use uuid::Uuid;

use crate::error::StoreError;
use crate::timestamp;
use crate::{Store, StoredImage};

/// The fields of a project that a caller supplies.
///
/// A project has no host field: every project in a store is on the host
/// whose plxd owns that store (decision record 0009).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectFields {
    pub name: String,
    pub repo_path: String,
    pub icon: Option<ProjectIcon>,
    /// The permission mode, `auto` or `bypass` (decision record 0042).
    pub permission: String,
}

/// A project's icon, stored as the client sent it and never read
/// (RYA-227, decision record 0032), with an optional uploaded image
/// (PLX-339, decision record 0038).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectIcon {
    pub name: String,
    pub color: Option<String>,
    pub image: Option<StoredImage>,
}

/// The icon columns, in [`icon_from_row`]'s order, that the projects and
/// repos tables share.
pub(crate) const ICON_COLUMNS: &str = "icon_name, icon_color, icon_image_type, icon_image_data";

/// The icon in [`ICON_COLUMNS`] starting at column `first`. A NULL name
/// means no icon, and a NULL image type means no image.
pub(crate) fn icon_from_row(row: &Row<'_>, first: usize) -> rusqlite::Result<Option<ProjectIcon>> {
    let name: Option<String> = row.get(first)?;
    let color: Option<String> = row.get(first + 1)?;
    let media_type: Option<String> = row.get(first + 2)?;
    let data: Option<String> = row.get(first + 3)?;
    Ok(name.map(|name| ProjectIcon {
        name,
        color,
        image: media_type
            .zip(data)
            .map(|(media_type, data)| StoredImage { media_type, data }),
    }))
}

/// The values of [`ICON_COLUMNS`] for `icon`, all NULL for none.
pub(crate) fn icon_columns(
    icon: Option<&ProjectIcon>,
) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>) {
    let image = icon.and_then(|icon| icon.image.as_ref());
    (
        icon.map(|icon| icon.name.as_str()),
        icon.and_then(|icon| icon.color.as_deref()),
        image.map(|image| image.media_type.as_str()),
        image.map(|image| image.data.as_str()),
    )
}

/// What [`Store::update_project`] changes. A field that is `None` stays as
/// it is, and `icon` replaces the whole icon.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectEdit {
    pub name: Option<String>,
    pub icon: Option<ProjectIcon>,
    pub permission: Option<String>,
}

/// A project row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub repo_path: String,
    pub icon: Option<ProjectIcon>,
    pub permission: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

/// A project row with its id and timestamps still as the TEXT SQLite stored
/// them, before the fallible conversion to [`Project`].
struct RawProject {
    id: String,
    name: String,
    repo_path: String,
    icon: Option<ProjectIcon>,
    permission: String,
    created_at: String,
    updated_at: String,
}

impl RawProject {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            name: row.get(1)?,
            repo_path: row.get(2)?,
            icon: icon_from_row(row, 6)?,
            permission: row.get(5)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
        })
    }

    fn matches(&self, fields: &ProjectFields) -> bool {
        self.name == fields.name
            && self.repo_path == fields.repo_path
            && self.icon == fields.icon
            && self.permission == fields.permission
    }

    fn into_project(self) -> Result<Project, StoreError> {
        Ok(Project {
            id: Uuid::parse_str(&self.id)?,
            name: self.name,
            repo_path: self.repo_path,
            icon: self.icon,
            permission: self.permission,
            created_at: timestamp::parse(&self.created_at)?,
            updated_at: timestamp::parse(&self.updated_at)?,
        })
    }
}

fn fetch_raw(conn: &Connection, id_text: &str) -> Result<Option<RawProject>, StoreError> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT id, name, repo_path, created_at, updated_at, permission, {ICON_COLUMNS}
                 FROM projects WHERE id = ?1"
            ),
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

        // A retry that already matches doesn't need the write lock: if
        // nothing else changes it, this snapshot is exactly what a full
        // locked check-then-insert would also return. If another writer is
        // mutating this row concurrently, that's true of any read anyway,
        // and the locked path below is still the one that runs when this
        // fast path can't already tell the answer.
        if let Some(existing) = fetch_raw(&self.conn, &id_text)?
            && existing.matches(fields)
        {
            return existing.into_project();
        }

        let now = timestamp::now();

        let (icon_name, icon_color, image_type, image_data) = icon_columns(fields.icon.as_ref());
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            &format!(
                "INSERT INTO projects (id, name, repo_path, created_at, updated_at, permission,
                     {ICON_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT (id) DO NOTHING"
            ),
            params![
                id_text,
                fields.name,
                fields.repo_path,
                now,
                fields.permission,
                icon_name,
                icon_color,
                image_type,
                image_data
            ],
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
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, name, repo_path, created_at, updated_at, permission, {ICON_COLUMNS}
             FROM projects
             ORDER BY created_at ASC, id ASC"
        ))?;
        let rows = stmt.query_map([], RawProject::from_row)?;

        let mut projects = Vec::new();
        for row in rows {
            projects.push(row?.into_project()?);
        }
        Ok(projects)
    }

    /// Renames project `id`, or sets its icon or permission mode, as `edit` says, and returns
    /// the project with whether anything changed. When nothing would
    /// change, it writes nothing.
    ///
    /// `repo_path` never changes, and `updated_at` stays as it is: a rename,
    /// a new icon, or a new mode is not activity (decision record 0032).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::NotFound`] if no project has `id`, or a
    /// database error.
    pub fn update_project(
        &mut self,
        id: Uuid,
        edit: &ProjectEdit,
    ) -> Result<(Project, bool), StoreError> {
        let id_text = id.to_string();
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut raw = fetch_raw(&tx, &id_text)?.ok_or(StoreError::NotFound { id })?;

        let mut changed = false;
        if let Some(name) = &edit.name
            && *name != raw.name
        {
            raw.name.clone_from(name);
            changed = true;
        }
        if edit.icon.is_some() && edit.icon != raw.icon {
            raw.icon.clone_from(&edit.icon);
            changed = true;
        }
        if let Some(permission) = &edit.permission
            && *permission != raw.permission
        {
            raw.permission.clone_from(permission);
            changed = true;
        }

        if changed {
            let (icon_name, icon_color, image_type, image_data) = icon_columns(raw.icon.as_ref());
            tx.execute(
                "UPDATE projects SET name = ?2, icon_name = ?3, icon_color = ?4,
                     icon_image_type = ?5, icon_image_data = ?6, permission = ?7
                 WHERE id = ?1",
                params![
                    id_text,
                    raw.name,
                    icon_name,
                    icon_color,
                    image_type,
                    image_data,
                    raw.permission
                ],
            )?;
            tx.commit()?;
        }
        Ok((raw.into_project()?, changed))
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
