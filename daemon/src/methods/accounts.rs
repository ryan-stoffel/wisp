//! `accounts/keys/add`, `accounts/keys/list`, and `accounts/keys/remove` (#117).
//!
//! The key itself is never logged and never reaches the project store or the event log: only
//! [`crate::keystore::KeyStore`] sees it, and [`mask_key`] turns it into the display form
//! (`sk-ant-...abcd`) that everything else, including this module's own logging, works with.

use std::sync::Arc;

use tracing::{error, info};
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AccountId, AccountsKeysAddParams, AccountsKeysAddResult, AccountsKeysListParams,
    AccountsKeysListResult, AccountsKeysRemoveParams, AccountsKeysRemoveResult, Provider,
};
use wisp_store::StoreError;

use super::Context;
use crate::keystore::KeyStore;
use crate::store::{self, account_store_error};

/// The longest `label` wispd accepts, in bytes.
const MAX_LABEL_BYTES: usize = 256;

/// The longest `key` wispd accepts, in bytes.
const MAX_KEY_BYTES: usize = 4096;

pub(crate) async fn add(
    context: &Context,
    params: AccountsKeysAddParams,
) -> Result<AccountsKeysAddResult, ErrorObject> {
    check(&params)?;
    let AccountsKeysAddParams {
        id,
        provider,
        label,
        key,
    } = params;
    let keys = Arc::clone(&context.daemon.keys);
    context
        .daemon
        .store
        .run(&context.cancel, move |db_store| {
            add_account(db_store, keys.as_ref(), id, provider, label, key.expose())
        })
        .await
}

/// The logic behind `accounts/keys/add`, apart from wispd's dedicated store thread, so a test can
/// run it directly and capture what it logs.
///
/// Idempotent on `id`: if a row already exists with the same `provider`, `label`, and masked key,
/// it is returned unchanged and the Keychain is not touched. Otherwise the key is stored in the
/// Keychain first, so a store row is never created for a key that failed to save.
fn add_account(
    db_store: &mut wisp_store::Store,
    keys: &dyn KeyStore,
    id: AccountId,
    provider: Provider,
    label: String,
    key: &str,
) -> Result<AccountsKeysAddResult, ErrorObject> {
    let masked_key = mask_key(key);
    let fields = store::account_fields(provider, label, masked_key);
    let uuid = id.into();
    if let Some(existing) = db_store
        .get_account(uuid)
        .map_err(|error| account_store_error(&error))?
    {
        return if existing.provider == fields.provider
            && existing.label == fields.label
            && existing.masked_key == fields.masked_key
        {
            Ok(AccountsKeysAddResult {
                account: store::key_account(existing)?,
            })
        } else {
            Err(account_store_error(&StoreError::IdConflict { id: uuid }))
        };
    }
    keys.set(id, key).map_err(|error| {
        error!(%error, "could not store a key in the keychain");
        ErrorObject::internal_error("the keychain failed")
    })?;
    let row = db_store
        .create_account(uuid, &fields)
        .map_err(|error| account_store_error(&error))?;
    let account = store::key_account(row)?;
    info!(account = %account.id, provider = ?account.provider, "added a key account");
    Ok(AccountsKeysAddResult { account })
}

pub(crate) async fn list(
    context: &Context,
    _: AccountsKeysListParams,
) -> Result<AccountsKeysListResult, ErrorObject> {
    context
        .daemon
        .store
        .run(&context.cancel, move |db_store| {
            let rows = db_store
                .list_accounts()
                .map_err(|error| account_store_error(&error))?;
            let accounts = rows
                .into_iter()
                .map(store::key_account)
                .collect::<Result<_, _>>()?;
            Ok(AccountsKeysListResult { accounts })
        })
        .await
}

pub(crate) async fn remove(
    context: &Context,
    params: AccountsKeysRemoveParams,
) -> Result<AccountsKeysRemoveResult, ErrorObject> {
    let AccountsKeysRemoveParams { id } = params;
    let keys = Arc::clone(&context.daemon.keys);
    context
        .daemon
        .store
        .run(&context.cancel, move |db_store| {
            let uuid = id.into();
            if db_store
                .get_account(uuid)
                .map_err(|error| account_store_error(&error))?
                .is_none()
            {
                return Err(account_store_error(&StoreError::NotFound { id: uuid }));
            }
            // The Keychain first, so a failed removal never leaves an account with no record but
            // a key that is still stored.
            keys.delete(id).map_err(|error| {
                error!(%error, "could not remove a key from the keychain");
                ErrorObject::internal_error("the keychain failed")
            })?;
            db_store
                .delete_account(uuid)
                .map_err(|error| account_store_error(&error))?;
            info!(account = %id, "removed a key account");
            Ok(AccountsKeysRemoveResult {})
        })
        .await
}

fn check(params: &AccountsKeysAddParams) -> Result<(), ErrorObject> {
    let AccountsKeysAddParams {
        provider,
        label,
        key,
        ..
    } = params;
    if matches!(provider, Provider::Unknown) {
        return Err(ErrorObject::invalid_params(
            "provider must be anthropic, openai, or cursor",
        ));
    }
    if label.trim().is_empty() {
        return Err(ErrorObject::invalid_params("label must not be empty"));
    }
    if label.len() > MAX_LABEL_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "label must be at most {MAX_LABEL_BYTES} bytes"
        )));
    }
    if label.contains('\0') {
        return Err(ErrorObject::invalid_params("label must not contain NUL"));
    }
    let key = key.expose();
    if key.is_empty() {
        return Err(ErrorObject::invalid_params("key must not be empty"));
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "key must be at most {MAX_KEY_BYTES} bytes"
        )));
    }
    if key.contains('\0') {
        return Err(ErrorObject::invalid_params("key must not contain NUL"));
    }
    Ok(())
}

/// Masks a key for display: its vendor prefix, up to and including the second `-` (such as
/// `sk-ant-`), plus its last 4 characters, such as `sk-ant-...abcd`. Falls back to the first 6
/// characters when the key has no such prefix, and to `...` alone when it is too short for a
/// prefix and suffix that do not overlap.
pub(crate) fn mask_key(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 4 {
        return "...".to_owned();
    }
    let last4: String = chars[chars.len() - 4..].iter().collect();
    let prefix_end = key
        .char_indices()
        .filter(|(_, c)| *c == '-')
        .take(2)
        .last()
        .map(|(index, c)| index + c.len_utf8());
    let mut prefix = prefix_end.map_or_else(String::new, |end| key[..end].to_owned());
    if prefix.is_empty() || prefix.chars().count() > 12 {
        prefix = chars.iter().take(6).collect();
    }
    format!("{prefix}...{last4}")
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex, PoisonError};

    use tracing_subscriber::fmt::MakeWriter;
    use wisp_protocol::{AccountId, Provider};

    use super::{add_account, mask_key};
    use crate::keystore::MemoryKeyStore;

    #[test]
    fn masks_a_typical_vendor_key() {
        assert_eq!(
            mask_key("sk-ant-api03-thisisathrowawaytestkeyabcd"),
            "sk-ant-...abcd"
        );
        assert_eq!(
            mask_key("sk-proj-averylongopenaikeywxyz"),
            "sk-proj-...wxyz"
        );
    }

    #[test]
    fn falls_back_to_a_leading_slice_with_no_recognizable_prefix() {
        let masked = mask_key("cursor_live_abcdefghijklmnopqrstuvwxyz");
        assert!(masked.starts_with("cursor"), "{masked}");
        assert!(masked.ends_with("...wxyz"), "{masked}");
    }

    #[test]
    fn very_short_keys_mask_to_dots() {
        assert_eq!(mask_key(""), "...");
        assert_eq!(mask_key("abcd"), "...");
    }

    #[derive(Clone)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl io::Write for SharedBuffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for SharedBuffer {
        type Writer = Self;

        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    /// Adding a key must never write it, in the clear, anywhere `tracing` can see: not in an
    /// `info!`, not in an `error!`, not in a `Debug` of the params.
    #[test]
    fn adding_a_key_never_logs_it() {
        let secret = "sk-ant-api03-thisisaveryrealsecretvalueabcd";
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_writer(SharedBuffer(Arc::clone(&buffer)))
            .with_ansi(false)
            .finish();

        let dir = tempfile::tempdir().unwrap();
        let mut db_store = wisp_store::Store::open(dir.path().join("wispd.sqlite3")).unwrap();
        let keys = MemoryKeyStore::new();

        tracing::subscriber::with_default(subscriber, || {
            add_account(
                &mut db_store,
                &keys,
                AccountId::generate(),
                Provider::Anthropic,
                "Personal".to_owned(),
                secret,
            )
            .expect("add should succeed");
        });

        let logged = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        assert!(
            !logged.contains(secret),
            "logged output must never contain the raw key: {logged}"
        );
        assert!(
            logged.contains("added a key account"),
            "sanity check that the add was actually logged: {logged}"
        );
    }
}
