//! `accounts/defaults/get` and `accounts/defaults/set` (#119): the per-host default account a
//! task's role falls back to when it doesn't name one outright. [`crate::routing`] is what
//! actually reads these to route a task; this module only lets the editor see and change them.

use tracing::error;
use wisp_protocol::jsonrpc::ErrorObject;
use wisp_protocol::{
    AccountsDefaultsGetParams, AccountsDefaultsGetResult, AccountsDefaultsSetParams, Role,
};
use wisp_store::{Store, StoreError};

use super::Context;
use crate::store::{account_choice, role_default, role_text};

pub(crate) async fn get(
    context: &Context,
    _: AccountsDefaultsGetParams,
) -> Result<AccountsDefaultsGetResult, ErrorObject> {
    context
        .daemon
        .store
        .run(&context.cancel, move |db_store| read_defaults(db_store))
        .await
}

pub(crate) async fn set(
    context: &Context,
    params: AccountsDefaultsSetParams,
) -> Result<AccountsDefaultsGetResult, ErrorObject> {
    let AccountsDefaultsSetParams { role, account } = params;
    let default = account.as_ref().map(role_default).transpose()?;
    context
        .daemon
        .store
        .run(&context.cancel, move |db_store| {
            db_store
                .set_role_default(role_text(role), default.as_ref())
                .map_err(|error| defaults_store_error(&error))?;
            read_defaults(db_store)
        })
        .await
}

/// Both roles' defaults, as they stand in the store right now.
fn read_defaults(db_store: &Store) -> Result<AccountsDefaultsGetResult, ErrorObject> {
    let coordinator = read_one(db_store, Role::Coordinator)?;
    let worker = read_one(db_store, Role::Worker)?;
    Ok(AccountsDefaultsGetResult {
        coordinator,
        worker,
    })
}

fn read_one(
    db_store: &Store,
    role: Role,
) -> Result<Option<wisp_protocol::AccountChoice>, ErrorObject> {
    db_store
        .get_role_default(role_text(role))
        .map_err(|error| defaults_store_error(&error))?
        .map(account_choice)
        .transpose()
}

fn defaults_store_error(error: &StoreError) -> ErrorObject {
    error!(error = %error, "the project store failed reading or writing a role default");
    ErrorObject::internal_error(format!("the project store failed: {error}"))
}

#[cfg(test)]
mod tests {
    use wisp_protocol::{AccountChoice, AccountId};
    use wisp_store::Store;

    use super::read_defaults;
    use crate::store::role_default;

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("wispd.sqlite3")).unwrap();
        (dir, store)
    }

    #[test]
    fn with_nothing_set_both_roles_are_absent() {
        let (_dir, store) = temp_store();
        let defaults = read_defaults(&store).unwrap();
        assert_eq!(defaults.coordinator, None);
        assert_eq!(defaults.worker, None);
    }

    #[test]
    fn each_role_keeps_its_own_default() {
        let (_dir, mut store) = temp_store();
        let subscription = AccountChoice::Subscription {
            backend: "claude".to_owned(),
        };
        let key = AccountChoice::Key {
            id: AccountId::generate(),
        };
        store
            .set_role_default("coordinator", Some(&role_default(&subscription).unwrap()))
            .unwrap();
        store
            .set_role_default("worker", Some(&role_default(&key).unwrap()))
            .unwrap();

        let defaults = read_defaults(&store).unwrap();
        assert_eq!(defaults.coordinator, Some(subscription));
        assert_eq!(defaults.worker, Some(key));
    }
}
