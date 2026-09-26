//! Per-host role defaults for account routing (#119).
//!
//! A task names an account outright, or falls back to its role's default. `accounts/defaults/get`
//! reads both roles' defaults; `accounts/defaults/set` changes one. Neither touches the Keychain:
//! an [`AccountChoice::Key`] only names an id from `accounts/keys/*` (#117).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::AccountId;

/// Which kind of run an account is chosen for (0004).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    /// The no-write planner, always on Claude Code or Codex.
    Coordinator,
    /// A subagent that edits a worktree.
    Worker,
}

/// An account a task can be routed to: the user's own login in a vendor CLI, or a stored key.
///
/// A newer wispd may send a kind this version does not know. Treat that as absent rather than end
/// a `switch` over this type in an exhaustiveness assertion.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AccountChoice {
    /// The user's own signed-in login in a vendor CLI (0004), named by the backend that runs it,
    /// such as `claude`.
    Subscription {
        /// The backend's name, as `host/health`'s running agents and wispd's logs use it.
        backend: String,
    },
    /// A key account from the Keychain (#117).
    Key {
        /// The key account's id.
        id: AccountId,
    },
    /// A kind this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// Params of `accounts/defaults/get`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountsDefaultsGetParams {}

/// Result of `accounts/defaults/get`, and of `accounts/defaults/set`: this host's default account
/// for each role, absent where none is set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountsDefaultsGetResult {
    /// The coordinator's default account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub coordinator: Option<AccountChoice>,
    /// A worker's default account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub worker: Option<AccountChoice>,
}

/// Params of `accounts/defaults/set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountsDefaultsSetParams {
    /// The role to set the default for.
    pub role: Role,
    /// The new default, or `None` to clear it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<AccountChoice>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AccountChoice, Role};

    #[test]
    fn roles_are_camel_case_strings() {
        for (role, name) in [(Role::Coordinator, "coordinator"), (Role::Worker, "worker")] {
            assert_eq!(serde_json::to_value(role).unwrap(), json!(name));
            assert_eq!(serde_json::from_value::<Role>(json!(name)).unwrap(), role);
        }
    }

    #[test]
    fn account_choices_tag_by_kind() {
        let subscription = AccountChoice::Subscription {
            backend: "claude".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&subscription).unwrap(),
            json!({"kind": "subscription", "backend": "claude"})
        );
        let key = AccountChoice::Key {
            id: super::AccountId::generate(),
        };
        let json = serde_json::to_value(&key).unwrap();
        assert_eq!(json["kind"], "key");
        assert_eq!(serde_json::from_value::<AccountChoice>(json).unwrap(), key);
    }

    #[test]
    fn an_unknown_kind_decodes_as_unknown() {
        let choice: AccountChoice =
            serde_json::from_value(json!({"kind": "quantum", "qubit": 1})).unwrap();
        assert_eq!(choice, AccountChoice::Unknown);
    }
}
