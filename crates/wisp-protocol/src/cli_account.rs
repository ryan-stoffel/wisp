//! Detected vendor CLIs (#114): `accounts/list` and `accounts/refresh`.
//!
//! Distinct from #117's `accounts/keys/*`, which manages stored API keys. This is read-only:
//! wispd never signs a CLI in, never touches its credential files, and only runs the status
//! commands decision record 0004 lists.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A vendor CLI wispd knows how to detect (0004).
///
/// A newer wispd may send kinds that are not listed here. Treat those as unknown, so a `switch`
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
/// A newer wispd may send kinds that are not listed here. Treat those as unknown, so a `switch`
/// over this type must not end in an exhaustiveness assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum AuthKind {
    /// The vendor's own subscription login (0004: Claude Max, `ChatGPT` Plus, Cursor Pro+).
    Subscription,
    /// An API key, whether from the Keychain (#117) or the CLI's own configuration.
    ApiKey,
    /// The CLI is signed in, but wispd could not tell how.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// One CLI's detected state.
///
/// Every field but `cli` and `installed` is best-effort: the status commands 0004 lists are
/// undocumented in places, so a field wispd could not read is `null` rather than a guess.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DetectedCli {
    /// Which CLI this is.
    pub cli: CliKind,
    /// Whether the binary resolves on the `PATH` wispd itself uses (#96).
    pub installed: bool,
    /// The resolved absolute path, when installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub path: Option<String>,
    /// The CLI's version, when the status command happened to expose it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// Whether the user is signed in. `null` when installed but wispd could not tell (a timeout,
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
    /// Every CLI wispd knows how to detect, in a stable order (`claude`, `codex`, `cursor`).
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
    /// Every CLI wispd knows how to detect, freshly probed.
    pub clis: Vec<DetectedCli>,
    /// When this probe ran.
    pub checked_at: Timestamp,
}
