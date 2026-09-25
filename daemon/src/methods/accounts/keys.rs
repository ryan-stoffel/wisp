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
    AccountsKeysListResult, AccountsKeysRemoveParams, AccountsKeysRemoveResult, ErrorKind,
    Provider,
};
use wisp_store::StoreError;

use crate::keystore::{KeyStore, KeyStoreError};
use crate::methods::Context;
use crate::store::{self, account_store_error};

/// The fewest bytes wispd accepts for a key. Every real Anthropic, `OpenAI`, or Cursor key is far
/// longer; this also keeps [`mask_key`] from having only a sliver of a short, possibly-fragment
/// key to work with.
const MIN_KEY_BYTES: usize = 20;

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
/// its *stored* key is also checked against `key` before this is treated as a retry, since a mask
/// is a lossy display form and two different keys can share one. Anything else with an existing
/// row is `idConflict`. A new row's key is stored in the Keychain first, so a row is never created
/// for a key that failed to save; if the store write then fails, the Keychain write is rolled
/// back, so a failed add never leaves a key with no record behind it either.
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
        let same_fields = existing.provider == fields.provider
            && existing.label == fields.label
            && existing.masked_key == fields.masked_key;
        if same_fields {
            let stored = keys.get(id).map_err(|error| {
                error!(%error, "could not read a key from the keychain");
                map_keychain_error(&error)
            })?;
            if stored.as_deref().map(String::as_str) == Some(key) {
                return Ok(AccountsKeysAddResult {
                    account: store::key_account(existing)?,
                });
            }
        }
        return Err(account_store_error(&StoreError::IdConflict { id: uuid }));
    }
    keys.set(id, key).map_err(|error| {
        error!(%error, "could not store a key in the keychain");
        map_keychain_error(&error)
    })?;
    let row = match db_store.create_account(uuid, &fields) {
        Ok(row) => row,
        Err(error) => {
            // The Keychain now holds a key with no record for it. Best-effort clean that up so a
            // failed add never leaves a key that the protocol can no longer see or remove.
            if let Err(cleanup_error) = keys.delete(id) {
                error!(
                    account = %id,
                    error = %cleanup_error,
                    "could not roll back a keychain write after the store failed"
                );
            }
            return Err(account_store_error(&error));
        }
    };
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
            remove_account(db_store, keys.as_ref(), id)
        })
        .await
}

/// The logic behind `accounts/keys/remove`, apart from wispd's dedicated store thread, so a test
/// can run it directly against a mock `KeyStore`.
fn remove_account(
    db_store: &mut wisp_store::Store,
    keys: &dyn KeyStore,
    id: AccountId,
) -> Result<AccountsKeysRemoveResult, ErrorObject> {
    let uuid = id.into();
    if db_store
        .get_account(uuid)
        .map_err(|error| account_store_error(&error))?
        .is_none()
    {
        return Err(account_store_error(&StoreError::NotFound { id: uuid }));
    }
    // The Keychain first, so a failed removal never leaves an account with no record but a key
    // that is still stored.
    keys.delete(id).map_err(|error| {
        error!(%error, "could not remove a key from the keychain");
        map_keychain_error(&error)
    })?;
    db_store
        .delete_account(uuid)
        .map_err(|error| account_store_error(&error))?;
    info!(account = %id, "removed a key account");
    Ok(AccountsKeysRemoveResult {})
}

/// The protocol error for a failed `KeyStore` call: `keychainUnavailable` when the Keychain is
/// locked or access was denied, so the editor can tell that apart from a bare internal error;
/// anything else stays a plain internal error, since its detail is not something to show.
fn map_keychain_error(error: &KeyStoreError) -> ErrorObject {
    if error.is_unavailable() {
        ErrorObject::wisp(
            ErrorKind::KeychainUnavailable,
            "the keychain is locked or access was denied",
        )
    } else {
        ErrorObject::internal_error("the keychain failed")
    }
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
    if key.len() < MIN_KEY_BYTES {
        return Err(ErrorObject::invalid_params(format!(
            "key must be at least {MIN_KEY_BYTES} bytes"
        )));
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

/// Known vendor key prefixes, checked longest first when more than one matches (`sk-ant-api03-`
/// before `sk-ant-` before `sk-`). A key whose start is not on this list shows no prefix at all:
/// [`mask_key`] never guesses at a prefix from a key's own bytes, since those bytes might be
/// secret material rather than a real vendor marker (for example a base64url key legitimately
/// contains `-`).
const KNOWN_KEY_PREFIXES: &[&str] = &[
    "sk-ant-api03-",
    "sk-ant-admin01-",
    "sk-ant-",
    "sk-proj-",
    "sk-svcacct-",
    "sk-admin-",
    "sk-",
    "key_",
];

/// The fewest characters that must remain after a recognized prefix (or from the start, when none
/// matches) before [`mask_key`] shows the key's last 4 characters. Below this, only the prefix (or
/// nothing) is shown, so a short key, or one barely longer than its own prefix, can't have most or
/// all of itself echoed back.
const MIN_CHARS_TO_REVEAL_SUFFIX: usize = 16;

/// Masks a key for display: a recognized vendor prefix, if any, plus the key's last 4 characters
/// once there is enough left unrevealed by that prefix to make that safe — for example
/// `sk-ant-api03-...abcd`. Otherwise it is `prefix...`, or `...` alone with no recognized prefix.
///
/// This never reveals more than 4 characters of a key beyond a known public prefix, and never the
/// whole key, regardless of the key's shape or length. wispd's own `check` already rejects a key
/// shorter than [`MIN_KEY_BYTES`], but this function makes no assumption about that.
pub(crate) fn mask_key(key: &str) -> String {
    let prefix = KNOWN_KEY_PREFIXES
        .iter()
        .copied()
        .filter(|candidate| key.starts_with(candidate))
        .max_by_key(|candidate| candidate.len())
        .unwrap_or("");
    let remaining = key[prefix.len()..].chars().count();
    if remaining < MIN_CHARS_TO_REVEAL_SUFFIX {
        return format!("{prefix}...");
    }
    let last4: String = {
        let mut chars: Vec<char> = key.chars().rev().take(4).collect();
        chars.reverse();
        chars.into_iter().collect()
    };
    format!("{prefix}...{last4}")
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex, PoisonError};

    use tracing_subscriber::fmt::MakeWriter;
    use wisp_protocol::jsonrpc::INVALID_PARAMS;
    use wisp_protocol::{AccountId, ErrorKind, Provider, RawKey};
    use wisp_store::Store;

    use super::{add_account, check, mask_key, remove_account};
    use crate::keystore::{KeyStore, MemoryKeyStore};

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("wispd.sqlite3")).unwrap();
        (dir, store)
    }

    fn raw_key(text: &str) -> RawKey {
        serde_json::from_value(serde_json::json!(text)).unwrap()
    }

    fn params(provider: Provider, label: &str, key: &str) -> super::AccountsKeysAddParams {
        super::AccountsKeysAddParams {
            id: AccountId::generate(),
            provider,
            label: label.to_owned(),
            key: raw_key(key),
        }
    }

    #[test]
    fn masks_reveal_at_most_4_characters_beyond_a_known_prefix_and_never_the_whole_key() {
        // From the review of this table: nothing here may show more than a known public prefix
        // plus 4 trailing characters, and a short key must not come back nearly whole.
        let cases = [
            ("abcde", "..."),
            ("abcdefghij", "..."),
            ("sk-ant-1234", "sk-ant-..."),
            ("sk-ab", "sk-..."),
            ("sk-ant-12345678", "sk-ant-..."),
            ("ghijklmnopqrstuvwxyz0123", "...0123"),
            (
                "sk-ant-api03-thisisathrowawaytestkeyabcd",
                "sk-ant-api03-...abcd",
            ),
            ("sk-proj-averylongopenaikeywxyz", "sk-proj-...wxyz"),
            ("sk-svcacct-averylongservicekey1234", "sk-svcacct-...1234"),
            (
                "sk-admin-averylongadminkeyforanthropic5678",
                "sk-admin-...5678",
            ),
            ("key_anunknownvendorbutknownprefixwxyz", "key_...wxyz"),
        ];
        for (input, expected) in cases {
            assert_eq!(mask_key(input), expected, "input {input:?}");
        }
    }

    #[test]
    fn no_recognized_prefix_shows_no_prefix() {
        let masked = mask_key("cursor_live_abcdefghijklmnopqrstuvwxyz");
        assert_eq!(masked, "...wxyz");
    }

    #[test]
    fn a_hyphenated_key_with_no_known_prefix_never_leaks_its_random_bytes_as_a_prefix() {
        // base64url keys legitimately contain '-'; none of it is a recognized vendor prefix, so
        // none of it should be echoed back as though it were one.
        let masked = mask_key("Ab3xY-9kQ-randomBase64urlLookingSecretValue");
        assert_eq!(masked, "...alue");
        assert!(!masked.contains("Ab3xY"), "{masked}");
    }

    #[test]
    fn very_short_keys_mask_to_dots() {
        assert_eq!(mask_key(""), "...");
        assert_eq!(mask_key("abcd"), "...");
    }

    #[test]
    fn check_rejects_an_unknown_provider() {
        let error = check(&params(
            Provider::Unknown,
            "Personal",
            "sk-ant-averylongkeyabcd1234",
        ))
        .unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS);
    }

    #[test]
    fn check_rejects_a_key_under_the_minimum_length() {
        let error = check(&params(Provider::Anthropic, "Personal", "short")).unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS);
        let longest_rejected = "a".repeat(super::MIN_KEY_BYTES - 1);
        assert!(check(&params(Provider::Anthropic, "Personal", &longest_rejected)).is_err());
        let shortest_accepted = "a".repeat(super::MIN_KEY_BYTES);
        assert!(check(&params(Provider::Anthropic, "Personal", &shortest_accepted)).is_ok());
    }

    #[test]
    fn removing_an_unknown_id_is_account_not_found() {
        let (_dir, mut db_store) = temp_store();
        let keys = MemoryKeyStore::new();
        let error = remove_account(&mut db_store, &keys, AccountId::generate()).unwrap_err();
        assert_eq!(error.wisp_data().unwrap().kind, ErrorKind::AccountNotFound);
    }

    #[test]
    fn remove_deletes_both_the_row_and_the_keychain_entry() {
        let (_dir, mut db_store) = temp_store();
        let keys = MemoryKeyStore::new();
        let id = AccountId::generate();
        add_account(
            &mut db_store,
            &keys,
            id,
            Provider::Anthropic,
            "Personal".to_owned(),
            "sk-ant-averylongthrowawaykeyabcd1234",
        )
        .unwrap();
        assert!(keys.get(id).unwrap().is_some());

        remove_account(&mut db_store, &keys, id).unwrap();

        assert_eq!(keys.get(id).unwrap(), None, "the key must be gone");
        assert_eq!(
            db_store.get_account(id.into()).unwrap(),
            None,
            "the row must be gone"
        );
    }

    #[test]
    fn a_retried_add_with_the_same_key_is_idempotent() {
        let (_dir, mut db_store) = temp_store();
        let keys = MemoryKeyStore::new();
        let id = AccountId::generate();
        let key = "sk-ant-averylongthrowawaykeyabcd1234";

        let first = add_account(
            &mut db_store,
            &keys,
            id,
            Provider::Anthropic,
            "Personal".to_owned(),
            key,
        )
        .unwrap();
        let second = add_account(
            &mut db_store,
            &keys,
            id,
            Provider::Anthropic,
            "Personal".to_owned(),
            key,
        )
        .unwrap();

        assert_eq!(first.account, second.account);
        assert_eq!(
            keys.get(id).unwrap().as_deref().map(String::as_str),
            Some(key)
        );
    }

    #[test]
    fn a_retry_with_a_different_label_is_an_id_conflict() {
        let (_dir, mut db_store) = temp_store();
        let keys = MemoryKeyStore::new();
        let id = AccountId::generate();
        add_account(
            &mut db_store,
            &keys,
            id,
            Provider::Anthropic,
            "Personal".to_owned(),
            "sk-ant-averylongthrowawaykeyabcd1234",
        )
        .unwrap();

        let error = add_account(
            &mut db_store,
            &keys,
            id,
            Provider::Anthropic,
            "Work".to_owned(),
            "sk-ant-averylongthrowawaykeyabcd1234",
        )
        .unwrap_err();
        assert_eq!(error.wisp_data().unwrap().kind, ErrorKind::IdConflict);
    }

    /// Regression test: two different keys can share a mask (same recognized prefix and last 4
    /// characters), so idempotency must not trust the mask alone.
    #[test]
    fn a_retry_with_a_different_key_that_shares_a_mask_is_an_id_conflict() {
        let (_dir, mut db_store) = temp_store();
        let keys = MemoryKeyStore::new();
        let id = AccountId::generate();
        let first_key = "sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAAabcd";
        let second_key = "sk-ant-api03-BBBBBBBBBBBBBBBBBBBBBBBBabcd";
        assert_eq!(
            mask_key(first_key),
            mask_key(second_key),
            "the test setup should give both keys the same mask"
        );
        assert_ne!(first_key, second_key);

        add_account(
            &mut db_store,
            &keys,
            id,
            Provider::Anthropic,
            "Personal".to_owned(),
            first_key,
        )
        .unwrap();

        let error = add_account(
            &mut db_store,
            &keys,
            id,
            Provider::Anthropic,
            "Personal".to_owned(),
            second_key,
        )
        .unwrap_err();
        assert_eq!(error.wisp_data().unwrap().kind, ErrorKind::IdConflict);
        assert_eq!(
            keys.get(id).unwrap().as_deref().map(String::as_str),
            Some(first_key),
            "the original key must be left untouched"
        );
    }

    #[test]
    fn a_failed_store_write_rolls_back_the_keychain_write() {
        let (dir, mut db_store) = temp_store();
        // Break the schema after opening, from a second connection to the same database, so the
        // next `create_account` fails immediately and deterministically: no fault-injecting mock
        // store needed, and nothing to wait out.
        {
            let raw = rusqlite::Connection::open(dir.path().join("wispd.sqlite3")).unwrap();
            raw.execute_batch("DROP TABLE accounts;").unwrap();
        }
        let keys = MemoryKeyStore::new();
        let id = AccountId::generate();

        let result = add_account(
            &mut db_store,
            &keys,
            id,
            Provider::Anthropic,
            "Personal".to_owned(),
            "sk-ant-averylongthrowawaykeyabcd1234",
        );

        assert!(result.is_err(), "the broken schema should fail the write");
        assert_eq!(
            keys.get(id).unwrap(),
            None,
            "a failed store write must not leave the key behind in the keychain"
        );
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

        let (_dir, mut db_store) = temp_store();
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
