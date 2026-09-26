use std::io;

use uuid::Uuid;

/// Errors returned by [`crate::Store`].
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// A project with this id already exists with different fields.
    #[error("project {id} already exists with different fields")]
    IdConflict {
        /// The id that was requested.
        id: Uuid,
    },

    /// No project exists with this id.
    #[error("no project with id {id}")]
    NotFound {
        /// The id that was requested.
        id: Uuid,
    },

    /// The database's recorded schema version is newer than this build of
    /// `wisp-store` knows how to migrate.
    #[error("database schema version {found} is newer than the {supported} this build supports")]
    UnsupportedSchemaVersion {
        /// The version recorded in the database.
        found: i64,
        /// The highest version this build knows how to migrate to.
        supported: i64,
    },

    /// SQLite reported a journal mode other than WAL after it was requested.
    #[error("failed to enable WAL journal mode (sqlite reports {0:?})")]
    JournalMode(String),

    /// A stored id is not a valid UUID.
    #[error("stored id is not a valid UUID: {0}")]
    InvalidId(#[from] uuid::Error),

    /// A stored role default's `account_kind` is not `subscription` or `key`, or its matching
    /// column (`backend` or `key_account_id`) is missing.
    #[error("stored default for role {role:?} has an invalid account_kind {kind:?}")]
    InvalidRoleDefault {
        /// The role whose row is corrupt.
        role: String,
        /// The `account_kind` value that was found.
        kind: String,
    },

    /// A stored timestamp is not valid RFC 3339.
    #[error("stored timestamp is invalid: {0}")]
    InvalidTimestamp(#[from] jiff::Error),

    /// Failed to prepare the database's parent directory.
    #[error("failed to prepare database directory: {0}")]
    Io(#[from] io::Error),

    /// SQLite returned an error.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}
