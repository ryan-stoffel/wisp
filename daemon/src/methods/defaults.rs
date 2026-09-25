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

/// Checks `account` before storing it: a `Key` must be a real row in `accounts` (#117), and a
/// `Subscription`'s backend must be one wispd knows. The check runs on the store's own thread, in
/// the same job that writes the default, since a `Key` check reads the `accounts` table.
pub(crate) async fn set(
    context: &Context,
    params: AccountsDefaultsSetParams,
) -> Result<AccountsDefaultsGetResult, ErrorObject> {
    let AccountsDefaultsSetParams { role, account } = params;
    context
        .daemon
        .store
        .run(&context.cancel, move |db_store| {
            let default = account
                .as_ref()
                .map(|choice| role_default(db_store, choice))
                .transpose()?;
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
    use wisp_protocol::jsonrpc::INVALID_PARAMS;
    use wisp_protocol::{AccountChoice, AccountId, Provider};
    use wisp_store::{AccountFields, Store};

    use super::read_defaults;
    use crate::store::{account_fields, role_default};

    fn temp_store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("wispd.sqlite3")).unwrap();
        (dir, store)
    }

    fn key_account_fields() -> AccountFields {
        account_fields(
            Provider::Anthropic,
            "Personal".to_owned(),
            "sk-ant-...abcd".to_owned(),
        )
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
        let key_id = AccountId::generate();
        store
            .create_account(key_id.into(), &key_account_fields())
            .unwrap();
        let subscription = AccountChoice::Subscription {
            backend: "claude".to_owned(),
        };
        let key = AccountChoice::Key { id: key_id };
        store
            .set_role_default(
                "coordinator",
                Some(&role_default(&store, &subscription).unwrap()),
            )
            .unwrap();
        store
            .set_role_default("worker", Some(&role_default(&store, &key).unwrap()))
            .unwrap();

        let defaults = read_defaults(&store).unwrap();
        assert_eq!(defaults.coordinator, Some(subscription));
        assert_eq!(defaults.worker, Some(key));
    }

    #[test]
    fn a_backend_wispd_does_not_know_is_refused() {
        let (_dir, store) = temp_store();
        let error = role_default(
            &store,
            &AccountChoice::Subscription {
                backend: "nope".to_owned(),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS);
    }

    #[test]
    fn a_key_account_that_does_not_exist_is_refused() {
        let (_dir, store) = temp_store();
        let error = role_default(
            &store,
            &AccountChoice::Key {
                id: AccountId::generate(),
            },
        )
        .unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS);
    }
}
