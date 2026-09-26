use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use ts_rs::TS;

use crate::{LogId, PROTOCOL_VERSION};

/// A range of protocol versions, both ends included. Its shape never changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolRange {
    /// The oldest version.
    pub min: u32,
    /// The newest version.
    pub max: u32,
}

impl ProtocolRange {
    /// The versions this crate speaks: 1 through `PROTOCOL_VERSION`.
    pub const SUPPORTED: Self = Self {
        min: 1,
        max: PROTOCOL_VERSION,
    };

    /// The newest version in both ranges, which the connection then speaks.
    #[must_use]
    pub fn highest_common(self, other: Self) -> Option<u32> {
        let min = self.min.max(other.min);
        let max = self.max.min(other.max);
        (min <= max).then_some(max)
    }
}

/// Params of `initialize`, the first request on every connection.
///
/// `protocol` and `capabilities` never change shape. To answer `incompatibleProtocol` even to a
/// client whose other params changed, decode `InitializeProtocol` before these.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    /// The protocol versions the client speaks.
    pub protocol: ProtocolRange,
    /// Who is connecting.
    pub client: ClientInfo,
    /// What the client supports.
    pub capabilities: Capabilities,
}

/// The part of `initialize`'s params that no protocol version changes: the client's range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeProtocol {
    /// The protocol versions the client speaks.
    pub protocol: ProtocolRange,
}

/// The client that sent `initialize`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    /// The client's name, such as `wisp` for the editor.
    pub name: String,
    /// The client's release version.
    pub version: String,
    /// Tells apart the machines of one user, for the local runner (M5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub machine_id: Option<String>,
}

/// Capabilities by name, such as `{"agents": {}}`. Each value holds that capability's options,
/// and is empty when it has none. The map never changes shape.
///
/// Later features are gated on a capability, so a newer editor still works with an older wispd.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Capabilities(pub BTreeMap<String, Map<String, Value>>);

/// Result of `initialize`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    /// The protocol version the connection speaks: the newest in both ranges.
    pub protocol: u32,
    /// wispd's release version.
    pub wispd: String,
    /// Identifies wispd's event log. It changes only when the log starts over, and then every
    /// `seq` the client holds is meaningless.
    pub log_id: LogId,
    /// What wispd supports.
    pub capabilities: Capabilities,
    /// The largest frame wispd accepts, in bytes, not counting the line ending.
    pub max_frame_bytes: u64,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{InitializeParams, InitializeProtocol, ProtocolRange};

    #[test]
    fn highest_common_picks_the_newest_shared_version() {
        let range = |min, max| ProtocolRange { min, max };
        assert_eq!(range(1, 1).highest_common(range(1, 1)), Some(1));
        assert_eq!(range(1, 3).highest_common(range(2, 5)), Some(3));
        assert_eq!(range(2, 5).highest_common(range(1, 3)), Some(3));
        assert_eq!(range(1, 1).highest_common(range(2, 3)), None);
        assert_eq!(range(4, 4).highest_common(range(1, 3)), None);
        assert_eq!(range(3, 1).highest_common(range(1, 3)), None);
    }

    #[test]
    fn a_future_client_still_yields_its_protocol_range() {
        let params = json!({
            "protocol": {"min": 2, "max": 4, "preferred": 4},
            "client": {"name": "wisp", "build": {"channel": "insiders"}},
            "capabilities": {"agents": {}},
            "workspace": "/"
        });
        let InitializeProtocol { protocol } = serde_json::from_value(params.clone()).unwrap();
        assert_eq!(protocol, ProtocolRange { min: 2, max: 4 });
        assert!(serde_json::from_value::<InitializeParams>(params).is_err());
    }
}
