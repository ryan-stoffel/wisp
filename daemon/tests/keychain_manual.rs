//! Manual verification of `KeychainStore` against the real login Keychain (#117).
//!
//! Ignored by default, so `scripts/ci/check-rust`'s `cargo test --workspace` (and CI) never
//! touches the real Keychain. Run it explicitly on a Mac:
//!
//! ```sh
//! cargo test -p wispd --test keychain_manual -- --ignored --nocapture
//! ```
//!
//! It uses a throwaway, test-only service name, never `wispd::keystore::SERVICE`, so it can't
//! disturb a real stored key, and it cleans up after itself. If it panics partway through, remove
//! the leftover item by hand:
//!
//! ```sh
//! security delete-generic-password -s io.github.ryan-stoffel.wisp.keychain-manual-test
//! ```

use std::process::Command;

use wisp_protocol::AccountId;
use wispd::keystore::{KeyStore, KeychainStore};

const TEST_SERVICE: &str = "io.github.ryan-stoffel.wisp.keychain-manual-test";

/// Whether `security find-generic-password` reports an item for `service`/`account`: the same
/// tool and check the issue's manual test calls for.
fn security_finds_it(service: &str, account: &str) -> bool {
    Command::new("security")
        .args(["find-generic-password", "-s", service, "-a", account])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[test]
#[ignore = "touches the real login Keychain; run manually with --ignored"]
fn add_list_and_remove_a_throwaway_key() {
    let store = KeychainStore::with_service(TEST_SERVICE);
    let account = AccountId::generate();
    let account_text = account.to_string();
    let key = "sk-ant-manual-test-throwaway-key-abcd";

    assert!(
        !security_finds_it(TEST_SERVICE, &account_text),
        "no leftover from an earlier run of this test"
    );
    assert_eq!(store.get(account).unwrap(), None);

    store.set(account, key).expect("store the throwaway key");
    assert!(
        security_finds_it(TEST_SERVICE, &account_text),
        "security find-generic-password should see the item wispd just wrote"
    );
    assert_eq!(
        store.get(account).unwrap().as_deref(),
        Some(key),
        "wispd should read back exactly what it stored"
    );

    store.delete(account).expect("remove the throwaway key");
    assert!(
        !security_finds_it(TEST_SERVICE, &account_text),
        "security find-generic-password should no longer see it after removal"
    );
    assert_eq!(store.get(account).unwrap(), None);
}
