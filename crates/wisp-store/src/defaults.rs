use rusqlite::{OptionalExtension, Row, params};
use uuid::Uuid;

use crate::Store;
use crate::error::StoreError;
use crate::timestamp;

/// A role's default account for routing (#119): the user's own login in a backend, or a key
/// account. Which role this is for is the caller's key into [`Store::set_role_default`] and
/// [`Store::get_role_default`], not part of the value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoleDefault {
    /// The user's own signed-in login in the named backend, such as `claude`.
    Subscription {
        /// The backend's name.
        backend: String,
    },
    /// A key account, by its id in the `accounts` table.
    Key {
        /// The key account's id.
        account_id: Uuid,
    },
}

fn from_row(
    role: &str,
    row: &Row<'_>,
) -> rusqlite::Result<Result<Option<RoleDefault>, StoreError>> {
    let kind: String = row.get(0)?;
    let backend: Option<String> = row.get(1)?;
    let key_account_id: Option<String> = row.get(2)?;
    let parsed = match (kind.as_str(), backend, key_account_id) {
        ("subscription", Some(backend), _) => Ok(Some(RoleDefault::Subscription { backend })),
        ("key", _, Some(id)) => match Uuid::parse_str(&id) {
            Ok(account_id) => Ok(Some(RoleDefault::Key { account_id })),
            Err(error) => Err(StoreError::InvalidId(error)),
        },
        (kind, _, _) => Err(StoreError::InvalidRoleDefault {
            role: role.to_owned(),
            kind: kind.to_owned(),
        }),
    };
    Ok(parsed)
}

impl Store {
    /// Sets `role`'s default account, replacing any existing one. `None` clears it.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn set_role_default(
        &mut self,
        role: &str,
        default: Option<&RoleDefault>,
    ) -> Result<(), StoreError> {
        let Some(default) = default else {
            self.conn
                .execute("DELETE FROM role_defaults WHERE role = ?1", params![role])?;
            return Ok(());
        };
        let (kind, backend, key_account_id): (&str, Option<&str>, Option<String>) = match default {
            RoleDefault::Subscription { backend } => ("subscription", Some(backend.as_str()), None),
            RoleDefault::Key { account_id } => ("key", None, Some(account_id.to_string())),
        };
        self.conn.execute(
            "INSERT INTO role_defaults (role, account_kind, backend, key_account_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (role) DO UPDATE SET
                account_kind = excluded.account_kind,
                backend = excluded.backend,
                key_account_id = excluded.key_account_id,
                updated_at = excluded.updated_at",
            params![role, kind, backend, key_account_id, timestamp::now()],
        )?;
        Ok(())
    }

    /// Reads `role`'s default account, or `None` if it has none set.
    ///
    /// # Errors
    ///
    /// Returns a database error, or [`StoreError::InvalidRoleDefault`] or
    /// [`StoreError::InvalidId`] if the stored row is corrupt.
    pub fn get_role_default(&self, role: &str) -> Result<Option<RoleDefault>, StoreError> {
        self.conn
            .query_row(
                "SELECT account_kind, backend, key_account_id FROM role_defaults WHERE role = ?1",
                params![role],
                |row| from_row(role, row),
            )
            .optional()?
            .unwrap_or(Ok(None))
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::RoleDefault;
    use crate::Store;
    use crate::error::StoreError;

    fn open() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("wispd.sqlite3")).unwrap();
        (dir, store)
    }

    #[test]
    fn a_role_with_no_default_reads_as_none() {
        let (_dir, store) = open();
        assert_eq!(store.get_role_default("coordinator").unwrap(), None);
    }

    #[test]
    fn setting_and_reading_round_trips_both_kinds() {
        let (_dir, mut store) = open();
        let subscription = RoleDefault::Subscription {
            backend: "claude".to_owned(),
        };
        store
            .set_role_default("coordinator", Some(&subscription))
            .unwrap();
        assert_eq!(
            store.get_role_default("coordinator").unwrap(),
            Some(subscription)
        );

        let key = RoleDefault::Key {
            account_id: Uuid::now_v7(),
        };
        store.set_role_default("worker", Some(&key)).unwrap();
        assert_eq!(store.get_role_default("worker").unwrap(), Some(key));
    }

    #[test]
    fn setting_again_replaces_the_previous_default() {
        let (_dir, mut store) = open();
        store
            .set_role_default(
                "worker",
                Some(&RoleDefault::Subscription {
                    backend: "claude".to_owned(),
                }),
            )
            .unwrap();
        let key = RoleDefault::Key {
            account_id: Uuid::now_v7(),
        };
        store.set_role_default("worker", Some(&key)).unwrap();
        assert_eq!(store.get_role_default("worker").unwrap(), Some(key));
    }

    #[test]
    fn setting_none_clears_it() {
        let (_dir, mut store) = open();
        store
            .set_role_default(
                "coordinator",
                Some(&RoleDefault::Subscription {
                    backend: "claude".to_owned(),
                }),
            )
            .unwrap();
        store.set_role_default("coordinator", None).unwrap();
        assert_eq!(store.get_role_default("coordinator").unwrap(), None);
    }

    #[test]
    fn a_corrupt_account_kind_is_reported() {
        let (_dir, store) = open();
        store
            .conn
            .execute(
                "INSERT INTO role_defaults (role, account_kind, backend, key_account_id, updated_at)
                 VALUES ('coordinator', 'gemini', NULL, NULL, '2026-09-24T12:00:00Z')",
                [],
            )
            .unwrap();
        let error = store.get_role_default("coordinator").unwrap_err();
        assert!(
            matches!(error, StoreError::InvalidRoleDefault { .. }),
            "{error:?}"
        );
    }
}
