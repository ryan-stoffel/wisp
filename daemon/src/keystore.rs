//! Where API keys live: the macOS login Keychain (#117).
//!
//! [`KeyStore`] is the interface. [`KeychainStore`] is the real login Keychain, one generic
//! password per account under a service name, via the `security-framework` crate.
//! [`MemoryKeyStore`] is an in-memory mock for tests. Only these two ever see a key in the clear,
//! and only for as long as it takes to hand it to `security-framework` or a caller; wisp's
//! project store and event log never do (0004, decision record 0009).

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use security_framework::base::Error as SecurityError;
use security_framework::passwords::{
    PasswordOptions, delete_generic_password, generic_password, set_generic_password,
};
use wisp_protocol::AccountId;
use zeroize::Zeroize;

/// The Keychain service name every real wisp key account is stored under: 0006's bundle id.
pub const SERVICE: &str = "io.github.ryan-stoffel.wisp";

/// `security_framework_sys::base::errSecItemNotFound`, kept as a local constant so this module
/// does not need `security-framework-sys` as a direct dependency for one status code.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// Where API keys are stored, keyed by account id.
///
/// An implementation must never pass a key to `tracing`, or write one anywhere but the real
/// Keychain (#117).
pub trait KeyStore: Send + Sync {
    /// Stores `key` for `account`, replacing any key already stored for it.
    ///
    /// # Errors
    ///
    /// If the store can't be written to.
    fn set(&self, account: AccountId, key: &str) -> Result<(), KeyStoreError>;

    /// The key stored for `account`, or `None` if there is none.
    ///
    /// # Errors
    ///
    /// If the store can't be read.
    fn get(&self, account: AccountId) -> Result<Option<String>, KeyStoreError>;

    /// Removes the key stored for `account`. Removing one that isn't there succeeds.
    ///
    /// # Errors
    ///
    /// If the store can't be written to.
    fn delete(&self, account: AccountId) -> Result<(), KeyStoreError>;
}

/// Why a [`KeyStore`] call failed.
#[derive(Debug, thiserror::Error)]
#[error("the keychain failed: {0}")]
pub struct KeyStoreError(#[from] SecurityError);

/// The user's login Keychain: one generic password per account, under a service name.
#[derive(Debug, Clone, Copy)]
pub struct KeychainStore {
    service: &'static str,
}

impl KeychainStore {
    /// The real wisp Keychain service, [`SERVICE`].
    #[must_use]
    pub const fn new() -> Self {
        Self { service: SERVICE }
    }

    /// A store under a different service name, so a test can't disturb a real stored key.
    #[must_use]
    pub const fn with_service(service: &'static str) -> Self {
        Self { service }
    }
}

impl Default for KeychainStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyStore for KeychainStore {
    fn set(&self, account: AccountId, key: &str) -> Result<(), KeyStoreError> {
        let account = account.to_string();
        let mut owned = key.to_owned();
        let result = set_generic_password(self.service, &account, owned.as_bytes());
        owned.zeroize();
        result.map_err(KeyStoreError)
    }

    fn get(&self, account: AccountId) -> Result<Option<String>, KeyStoreError> {
        let account = account.to_string();
        let options = PasswordOptions::new_generic_password(self.service, &account);
        match generic_password(options) {
            Ok(mut bytes) => {
                let key = String::from_utf8_lossy(&bytes).into_owned();
                bytes.zeroize();
                Ok(Some(key))
            }
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(error) => Err(KeyStoreError(error)),
        }
    }

    fn delete(&self, account: AccountId) -> Result<(), KeyStoreError> {
        let account = account.to_string();
        match delete_generic_password(self.service, &account) {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()),
            Err(error) => Err(KeyStoreError(error)),
        }
    }
}

/// An in-memory [`KeyStore`], for tests. Holds no reference to the real Keychain.
#[derive(Debug, Default)]
pub struct MemoryKeyStore {
    keys: Mutex<HashMap<AccountId, String>>,
}

impl MemoryKeyStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeyStore for MemoryKeyStore {
    fn set(&self, account: AccountId, key: &str) -> Result<(), KeyStoreError> {
        self.keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(account, key.to_owned());
        Ok(())
    }

    fn get(&self, account: AccountId) -> Result<Option<String>, KeyStoreError> {
        Ok(self
            .keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&account)
            .cloned())
    }

    fn delete(&self, account: AccountId) -> Result<(), KeyStoreError> {
        self.keys
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use wisp_protocol::AccountId;

    use super::{KeyStore, MemoryKeyStore};

    #[test]
    fn a_key_round_trips_through_the_mock_store() {
        let store = MemoryKeyStore::new();
        let account = AccountId::generate();
        assert_eq!(store.get(account).unwrap(), None);

        store.set(account, "sk-ant-secret").unwrap();
        assert_eq!(
            store.get(account).unwrap().as_deref(),
            Some("sk-ant-secret")
        );

        store.set(account, "sk-ant-replacement").unwrap();
        assert_eq!(
            store.get(account).unwrap().as_deref(),
            Some("sk-ant-replacement"),
            "setting again replaces the stored key"
        );

        store.delete(account).unwrap();
        assert_eq!(store.get(account).unwrap(), None);
    }

    #[test]
    fn deleting_a_key_that_was_never_set_succeeds() {
        let store = MemoryKeyStore::new();
        store.delete(AccountId::generate()).unwrap();
    }

    #[test]
    fn accounts_are_independent() {
        let store = MemoryKeyStore::new();
        let (a, b) = (AccountId::generate(), AccountId::generate());
        store.set(a, "key-a").unwrap();
        store.set(b, "key-b").unwrap();
        store.delete(a).unwrap();
        assert_eq!(store.get(a).unwrap(), None);
        assert_eq!(store.get(b).unwrap().as_deref(), Some("key-b"));
    }
}
