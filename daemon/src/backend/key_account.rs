//! Turning a stored key account into a run's [`Credential`] (#118).
//!
//! A key account (#117) names a [`Provider`] and an [`AccountId`]; the key itself lives in the
//! [`KeyStore`]. [`resolve`] reads it at the moment a run is about to start and wraps it as the
//! [`Credential`] its backend takes. The caller drops the result once
//! [`Backend::start`](super::Backend::start) has spawned the CLI: [`ApiKey`]'s `Drop` zeroizes
//! the key itself, and the copies the spawn path makes of it along the way
//! (`backend::process::Environment`'s entries, and `spawn_session`'s own buffers) zeroize
//! themselves the same way once each is done with its copy.
//!
//! Only Anthropic has a backend today (Claude Code, #116), so [`resolve`] only has that one arm.
//! `OpenAI` (#122) and Cursor (#123) are wired the same way once their backends exist; until then
//! this is the seam: add a `Provider::Openai` or `Provider::Cursor` arm here, the same shape as
//! Anthropic's.

use wisp_protocol::{AccountId, Provider};

use super::{ApiKey, Credential};
use crate::keystore::{KeyStore, KeyStoreError};

/// Reads `account`'s key from `store` and wraps it as the [`Credential`] `provider`'s backend
/// takes.
///
/// # Errors
///
/// [`KeyAccountError::KeychainUnavailable`] if `account` has no key, or the Keychain is locked or
/// denies access. wispd can't tell those apart without prompting, and either way the fix is the
/// same: unlock the Keychain, or add the key again. [`KeyAccountError::NoBackend`] if `provider`
/// has no backend yet. [`KeyAccountError::Keychain`] if the Keychain failed some other way.
pub fn resolve(
    store: &dyn KeyStore,
    provider: Provider,
    account: AccountId,
) -> Result<Credential, KeyAccountError> {
    match provider {
        Provider::Anthropic => {
            let key = match store.get(account) {
                Ok(Some(key)) => key,
                Ok(None) => return Err(KeyAccountError::KeychainUnavailable),
                Err(error) if error.is_unavailable() => {
                    return Err(KeyAccountError::KeychainUnavailable);
                }
                Err(error) => return Err(KeyAccountError::Keychain(error)),
            };
            Ok(Credential::ApiKey(ApiKey::new(key.to_string())))
        }
        Provider::Openai | Provider::Cursor | Provider::Unknown => {
            Err(KeyAccountError::NoBackend(provider))
        }
    }
}

/// Why a key account's [`Credential`] could not be resolved.
#[derive(Debug, thiserror::Error)]
pub enum KeyAccountError {
    /// `account` has no key in the Keychain, or the Keychain is locked or denied access.
    #[error("the keychain has no usable key for this account")]
    KeychainUnavailable,
    /// `provider` has no backend yet.
    #[error("{0:?} has no backend yet")]
    NoBackend(Provider),
    /// The Keychain failed in some other way.
    #[error("the keychain failed: {0}")]
    Keychain(#[source] KeyStoreError),
}

#[cfg(test)]
mod tests {
    use wisp_protocol::{AccountId, Provider};

    use super::{KeyAccountError, resolve};
    use crate::backend::Credential;
    use crate::keystore::{KeyStore, KeyStoreError, MemoryKeyStore};

    #[test]
    fn a_stored_key_resolves_to_an_api_key_credential() {
        let store = MemoryKeyStore::new();
        let account = AccountId::generate();
        store.set(account, "sk-ant-secret").unwrap();
        let Credential::ApiKey(key) = resolve(&store, Provider::Anthropic, account).unwrap() else {
            panic!("expected an API key credential");
        };
        assert_eq!(key.expose(), "sk-ant-secret");
    }

    #[test]
    fn a_missing_key_is_keychain_unavailable() {
        let store = MemoryKeyStore::new();
        let error = resolve(&store, Provider::Anthropic, AccountId::generate()).unwrap_err();
        assert!(matches!(error, KeyAccountError::KeychainUnavailable));
    }

    #[test]
    fn a_locked_keychain_is_keychain_unavailable() {
        // security_framework_sys::base::errSecInteractionNotAllowed: nothing can unlock the
        // Keychain to answer, such as a headless session (0004, 0007, #91).
        const ERR_SEC_INTERACTION_NOT_ALLOWED: i32 = -25308;

        struct LockedStore;
        impl KeyStore for LockedStore {
            fn set(&self, _account: AccountId, _key: &str) -> Result<(), KeyStoreError> {
                unreachable!("resolve never writes")
            }

            fn get(
                &self,
                _account: AccountId,
            ) -> Result<Option<zeroize::Zeroizing<String>>, KeyStoreError> {
                Err(KeyStoreError::from(security_framework::base::Error::from(
                    ERR_SEC_INTERACTION_NOT_ALLOWED,
                )))
            }

            fn delete(&self, _account: AccountId) -> Result<(), KeyStoreError> {
                unreachable!("resolve never writes")
            }
        }

        let error = resolve(&LockedStore, Provider::Anthropic, AccountId::generate()).unwrap_err();
        assert!(matches!(error, KeyAccountError::KeychainUnavailable));
    }

    #[test]
    fn providers_with_no_backend_yet_are_refused() {
        let store = MemoryKeyStore::new();
        for provider in [Provider::Openai, Provider::Cursor, Provider::Unknown] {
            let error = resolve(&store, provider, AccountId::generate()).unwrap_err();
            assert!(
                matches!(error, KeyAccountError::NoBackend(_)),
                "{provider:?}"
            );
        }
    }
}
