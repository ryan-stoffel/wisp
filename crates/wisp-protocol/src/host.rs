use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ProtocolRange;

/// Params of `host/health`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostHealthParams {}

/// Result of `host/health`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct HostHealthResult {
    /// Seconds since wispd started.
    pub uptime_seconds: u64,
    /// Whether wispd can read and write its project store.
    pub store: StoreState,
    /// Agents running on this host. Always 0 before M3.
    pub running_agents: u32,
}

/// The state of wispd's project store.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum StoreState {
    /// The store works.
    Ok,
    /// The store cannot be read or written, so project methods fail.
    Unavailable,
    /// A state this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// Params of `host/version`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostVersionParams {}

/// Result of `host/version`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct HostVersionResult {
    /// wispd's release version.
    pub wispd: String,
    /// The protocol versions wispd speaks.
    pub protocol: ProtocolRange,
    /// The operating system and its version, such as `macOS 27.0`.
    pub os: String,
    /// The CPU architecture, such as `aarch64`.
    pub arch: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{HostHealthParams, StoreState};

    #[test]
    fn unknown_store_states_decode_as_unknown() {
        assert_eq!(
            serde_json::from_value::<StoreState>(json!("migrating")).unwrap(),
            StoreState::Unknown
        );
        assert_eq!(serde_json::to_value(StoreState::Ok).unwrap(), json!("ok"));
    }

    #[test]
    fn empty_params_accept_future_options() {
        assert_eq!(
            serde_json::from_value::<HostHealthParams>(json!({"verbose": true})).unwrap(),
            HostHealthParams {}
        );
        assert_eq!(
            serde_json::to_value(HostHealthParams {}).unwrap(),
            json!({})
        );
    }
}
