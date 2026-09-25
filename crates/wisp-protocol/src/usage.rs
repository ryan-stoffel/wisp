use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Params of `usage/get`.
///
/// Empty: wispd has no account registry yet (#114, #117, #118 are still open, and #113's
/// `AccountRef` is already just a caller-supplied string), so it reports every account id it has
/// recorded usage or limits for.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageGetParams {}

/// Result of `usage/get`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageGetResult {
    /// Every account wispd has recorded usage or limits for, in no particular order.
    pub accounts: Vec<AccountUsage>,
}

/// One account's usage and latest limit windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    /// wispd's id for the account (#114, #117).
    pub account_id: String,
    /// Tokens and cost used today, local time on this host.
    pub today: UsagePeriod,
    /// Tokens and cost used this week (Monday to now), local time on this host.
    pub week: UsagePeriod,
    /// The account's limit windows, as last reported. Empty when the vendor reports none (0004:
    /// Cursor's headless output has no usage API).
    pub limits: Vec<UsageLimitWindow>,
}

/// Tokens and cost over a period.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsagePeriod {
    /// Input tokens, not counting cache reads and writes.
    pub input_tokens: u64,
    /// Output tokens, including reasoning.
    pub output_tokens: u64,
    /// Input tokens read from the prompt cache.
    pub cache_read_tokens: u64,
    /// Input tokens written to the prompt cache.
    pub cache_write_tokens: u64,
    /// The cost, when the vendor reports one for this account in the period. Absent, not zero,
    /// when it never does (0004: Codex and Cursor report no cost).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd_micros: Option<u64>,
}

/// One of an account's limit windows, as a vendor last reported it (0004's `rate_limit_event` and
/// Codex's `account/rateLimits/read`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitWindow {
    /// The vendor's name for the window, such as `five_hour`, `seven_day`, `primary`, or
    /// `secondary`.
    pub window: String,
    /// How much of the window is used, from 0 to 100, when the vendor says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_percent: Option<f64>,
    /// When the window resets, when the vendor says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
    /// When wispd captured this snapshot.
    pub captured_at: Timestamp,
}
