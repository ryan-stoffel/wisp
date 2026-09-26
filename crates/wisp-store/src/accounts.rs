use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use uuid::Uuid;

use crate::Store;
use crate::error::StoreError;
use crate::timestamp;

/// The fields of a key account that a caller supplies.
///
/// The account's key is not one of them: it lives only in the Keychain (#117), never in this
/// database. `masked_key` is its display form, such as `sk-ant-...abcd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountFields {
    pub provider: String,
    pub label: String,
    pub masked_key: String,
}

/// A key account row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: Uuid,
    pub provider: String,
    pub label: String,
    pub masked_key: String,
    pub created_at: Timestamp,
}

/// An account row with its id and timestamp still as the TEXT SQLite stored them, before the
/// fallible conversion to [`Account`].
struct RawAccount {
    id: String,
    provider: String,
    label: String,
    masked_key: String,
    created_at: String,
}

impl RawAccount {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            provider: row.get(1)?,
            label: row.get(2)?,
            masked_key: row.get(3)?,
            created_at: row.get(4)?,
        })
    }

    fn matches(&self, fields: &AccountFields) -> bool {
        self.provider == fields.provider
            && self.label == fields.label
            && self.masked_key == fields.masked_key
    }

    fn into_account(self) -> Result<Account, StoreError> {
        Ok(Account {
            id: Uuid::parse_str(&self.id)?,
            provider: self.provider,
            label: self.label,
            masked_key: self.masked_key,
            created_at: timestamp::parse(&self.created_at)?,
        })
    }
}

fn fetch_raw(conn: &Connection, id_text: &str) -> Result<Option<RawAccount>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT id, provider, label, masked_key, created_at
             FROM accounts WHERE id = ?1",
            params![id_text],
            RawAccount::from_row,
        )
        .optional()?)
}

impl Store {
    /// Creates a key account owned by `id`.
    ///
    /// If an account with `id` already exists with the same `fields`, this returns that existing
    /// row unchanged instead of creating a second one. If it exists with different fields, it
    /// fails with [`StoreError::IdConflict`] rather than overwrite it.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::IdConflict`] as described above, or a database error.
    pub fn create_account(
        &mut self,
        id: Uuid,
        fields: &AccountFields,
    ) -> Result<Account, StoreError> {
        let id_text = id.to_string();

        // See `create_project`'s fast path: a retry that already matches doesn't need the write
        // lock.
        if let Some(existing) = fetch_raw(&self.conn, &id_text)?
            && existing.matches(fields)
        {
            return existing.into_account();
        }

        let now = timestamp::now();

        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "INSERT INTO accounts (id, provider, label, masked_key, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (id) DO NOTHING",
            params![
                id_text,
                fields.provider,
                fields.label,
                fields.masked_key,
                now
            ],
        )?;
        let created = tx.changes() == 1;

        let raw = fetch_raw(&tx, &id_text)?.ok_or(StoreError::NotFound { id })?;

        if !created && !raw.matches(fields) {
            return Err(StoreError::IdConflict { id });
        }

        tx.commit()?;
        raw.into_account()
    }

    /// Reads a key account by id.
    ///
    /// # Errors
    ///
    /// Returns a database error, or an error if the stored id or timestamp is corrupt.
    pub fn get_account(&self, id: Uuid) -> Result<Option<Account>, StoreError> {
        fetch_raw(&self.conn, &id.to_string())?
            .map(RawAccount::into_account)
            .transpose()
    }

    /// Lists every key account, oldest first.
    ///
    /// # Errors
    ///
    /// Returns a database error, or an error if a stored id or timestamp is corrupt.
    pub fn list_accounts(&self) -> Result<Vec<Account>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, provider, label, masked_key, created_at
             FROM accounts
             ORDER BY created_at ASC, id ASC",
        )?;
        let rows = stmt.query_map([], RawAccount::from_row)?;
        rows.map(|row| row?.into_account()).collect()
    }

    /// Deletes a key account by id, if it exists.
    ///
    /// Returns whether a row was deleted.
    ///
    /// # Errors
    ///
    /// Returns a database error.
    pub fn delete_account(&self, id: Uuid) -> Result<bool, StoreError> {
        let changed = self.conn.execute(
            "DELETE FROM accounts WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(changed > 0)
    }
}
