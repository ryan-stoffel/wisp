//! Detected vendor CLIs (#114): `accounts/list` and `accounts/refresh`. Also the GitHub CLI
//! (PLX-336): `github/status`, and setting it up (PLX-423, 0050): `github/install`,
//! `github/signIn`, and `github/signInCancel`.
//!
//! Distinct from #117's `accounts/keys/*`, which manages stored API keys. The vendor CLIs are
//! read-only: plxd never signs one in, never touches its credential files, and only runs the
//! status commands decision record 0004 lists. `gh` is the one exception (0050): plxd may install
//! it and start its sign-in, and `gh` keeps the token.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A vendor CLI plxd knows how to detect (0004).
///
/// A newer plxd may send kinds that are not listed here. Treat those as unknown, so a `switch`
/// over this type must not end in an exhaustiveness assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum CliKind {
    /// Claude Code, the `claude` binary.
    Claude,
    /// Codex CLI, the `codex` binary.
    Codex,
    /// Cursor CLI, the `agent` binary (legacy name `cursor-agent`).
    Cursor,
    /// A CLI this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// How a signed-in CLI authenticates.
///
/// A newer plxd may send kinds that are not listed here. Treat those as unknown, so a `switch`
/// over this type must not end in an exhaustiveness assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AuthKind {
    /// The vendor's own subscription login (0004: Claude Max, `ChatGPT` Plus, Cursor Pro+).
    Subscription,
    /// An API key, whether from the Keychain (#117) or the CLI's own configuration.
    ApiKey,
    /// The CLI is signed in, but plxd could not tell how.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// One CLI's detected state.
///
/// Every field but `cli` and `installed` is best-effort: the status commands 0004 lists are
/// undocumented in places, so a field plxd could not read is `null` rather than a guess.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DetectedCli {
    /// Which CLI this is.
    pub cli: CliKind,
    /// Whether the binary resolves on the `PATH` plxd itself uses (#96).
    pub installed: bool,
    /// The resolved absolute path, when installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// The CLI's version, when the status command happened to expose it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// Whether the user is signed in. `null` when installed but plxd could not tell (a timeout,
    /// unparseable output, or an exit code 0004 doesn't document).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub signed_in: Option<bool>,
    /// How a signed-in CLI authenticates. Only set when `signedIn` is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub auth_kind: Option<AuthKind>,
    /// The subscription plan or tier, where the status commands expose it (0004: undocumented
    /// for all three vendors).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan: Option<String>,
    /// Why a field above is missing or uncertain, such as `"timed out after 5s"`. Never set on a
    /// clean read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// Params of `accounts/list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountsListParams {}

/// Result of `accounts/list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountsListResult {
    /// Every CLI plxd knows how to detect, in a stable order (`claude`, `codex`, `cursor`).
    pub clis: Vec<DetectedCli>,
    /// When these results were read. `accounts/list` may answer from a short-lived cache;
    /// `accounts/refresh` always sets this to the time of a fresh probe.
    pub checked_at: Timestamp,
}

/// Params of `accounts/refresh`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountsRefreshParams {}

/// Result of `accounts/refresh`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountsRefreshResult {
    /// Every CLI plxd knows how to detect, freshly probed.
    pub clis: Vec<DetectedCli>,
    /// When this probe ran.
    pub checked_at: Timestamp,
}

/// Params of `github/status`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubStatusParams {}

/// The GitHub CLI (`gh`) on the host: the result of `github/status`.
///
/// Every field but `installed` and `checkedAt` is best-effort: a field plxd could not read is
/// absent rather than a guess.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubStatus {
    /// Whether `gh` resolves on the `PATH` plxd itself uses (#96).
    pub installed: bool,
    /// Its version, such as `2.100.0`, from `gh --version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// Whether `gh` is signed in to github.com. Absent when installed but plxd could not tell (a
    /// timeout, or an exit code `gh auth status` doesn't use).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub signed_in: Option<bool>,
    /// The signed-in github.com login, when `gh auth status` names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<String>,
    /// Why a field above is missing, such as `"timed out after 5s"`. Never set on a clean read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
    /// Whether the `gh` in use is the copy `github/install` put in plxd's data folder, rather than
    /// one the user installed (PLX-423).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub managed: bool,
    /// Whether `github/install` is downloading and unpacking `gh` now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub installing: bool,
    /// The sign-in `github/signIn` started, until `gh auth login` exits and `gh auth setup-git`
    /// has run after it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub signing_in: Option<GithubSignIn>,
    /// What went wrong with the last install or sign-in plxd ran: why it failed, or why `gh auth
    /// setup-git` failed after a sign-in that worked. Cleared when the next one starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub setup_note: Option<String>,
    /// When plxd read this.
    pub checked_at: Timestamp,
}

/// Params of `github/install`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubInstallParams {}

/// Params of `github/signIn`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubSignInParams {}

/// A pending `gh auth login`: the one-time code to enter on GitHub's device page. The result of
/// `github/signIn`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubSignIn {
    /// The one-time code, such as `AA17-58F5`.
    pub code: String,
    /// The page to enter it on, `https://github.com/login/device`.
    pub url: String,
    /// When plxd stops waiting: GitHub's device codes last 15 minutes.
    pub expires_at: Timestamp,
}

/// Params of `github/signInCancel`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubSignInCancelParams {}

/// Result of `github/signInCancel`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct GithubSignInCancelResult {}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{AuthKind, CliKind};

    #[test]
    fn cli_kinds_are_camel_case_strings_and_unknown_ones_decode_as_unknown() {
        for (kind, name) in [
            (CliKind::Claude, "claude"),
            (CliKind::Codex, "codex"),
            (CliKind::Cursor, "cursor"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<CliKind>(json!(name)).unwrap(),
                kind
            );
        }
        assert_eq!(
            serde_json::from_value::<CliKind>(json!("gemini-cli")).unwrap(),
            CliKind::Unknown
        );
    }

    #[test]
    fn auth_kinds_are_camel_case_strings_and_unknown_ones_decode_as_unknown() {
        for (kind, name) in [
            (AuthKind::Subscription, "subscription"),
            (AuthKind::ApiKey, "apiKey"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<AuthKind>(json!(name)).unwrap(),
                kind
            );
        }
        assert_eq!(
            serde_json::from_value::<AuthKind>(json!("sso")).unwrap(),
            AuthKind::Unknown
        );
    }
}
