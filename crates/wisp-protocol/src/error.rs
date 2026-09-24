use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::ProtocolRange;
use crate::jsonrpc::{ErrorObject, WISP_ERROR};

/// What went wrong, in a wisp error's `data.kind`. Receivers match on it, never on the message.
///
/// A newer wispd may send kinds that are not listed here. Treat those as unknown errors, so a
/// `switch` over this type must not end in an exhaustiveness assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// A request came before `initialize`.
    NotInitialized,
    /// The client's protocol range does not overlap wispd's. Its `detail` is an
    /// `IncompatibleProtocolDetail` in every protocol version.
    IncompatibleProtocol,
    /// The events after the requested `seq` are gone or too many to replay. Reload the snapshots
    /// and subscribe again.
    ResyncRequired,
    /// No project has the given id.
    ProjectNotFound,
    /// A create reused an existing id with different params.
    IdConflict,
    /// A kind this version does not know yet.
    #[serde(other)]
    #[ts(skip)]
    Unknown,
}

/// The `data` of a wisp error (code -32000).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ErrorData {
    /// What went wrong.
    pub kind: ErrorKind,
    /// More about it, in a shape that depends on `kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<Value>,
}

/// The `detail` of `incompatibleProtocol`. Its shape never changes, so every editor can read it
/// from every wispd.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct IncompatibleProtocolDetail {
    /// The versions the client asked for.
    pub requested: ProtocolRange,
    /// The versions wispd speaks.
    pub supported: ProtocolRange,
    /// wispd's release version, so the editor can say which side to update.
    pub wispd: String,
}

impl ErrorObject {
    /// A wisp error of `kind`, with no detail.
    pub fn wisp(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            code: WISP_ERROR,
            message: message.into(),
            data: serde_json::to_value(ErrorData { kind, detail: None }).ok(),
        }
    }

    /// The `incompatibleProtocol` answer to an `initialize` whose range does not overlap.
    #[must_use]
    pub fn incompatible_protocol(detail: &IncompatibleProtocolDetail) -> Self {
        let IncompatibleProtocolDetail {
            requested,
            supported,
            wispd,
        } = detail;
        let message = format!(
            "wispd {wispd} speaks protocol versions {} to {}, but the client asked for {} to {}",
            supported.min, supported.max, requested.min, requested.max,
        );
        let data = ErrorData {
            kind: ErrorKind::IncompatibleProtocol,
            detail: serde_json::to_value(detail).ok(),
        };
        Self {
            code: WISP_ERROR,
            message,
            data: serde_json::to_value(data).ok(),
        }
    }

    /// The error's data, when it is a wisp error.
    #[must_use]
    pub fn wisp_data(&self) -> Option<ErrorData> {
        if self.code != WISP_ERROR {
            return None;
        }
        ErrorData::deserialize(self.data.as_ref()?).ok()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{ErrorData, ErrorKind, IncompatibleProtocolDetail};
    use crate::ProtocolRange;
    use crate::jsonrpc::{ErrorObject, INTERNAL_ERROR, WISP_ERROR};

    #[test]
    fn kinds_are_camel_case_strings() {
        for (kind, name) in [
            (ErrorKind::NotInitialized, "notInitialized"),
            (ErrorKind::IncompatibleProtocol, "incompatibleProtocol"),
            (ErrorKind::ResyncRequired, "resyncRequired"),
            (ErrorKind::ProjectNotFound, "projectNotFound"),
            (ErrorKind::IdConflict, "idConflict"),
        ] {
            assert_eq!(serde_json::to_value(kind).unwrap(), json!(name));
            assert_eq!(
                serde_json::from_value::<ErrorKind>(json!(name)).unwrap(),
                kind
            );
        }
    }

    #[test]
    fn unknown_kinds_decode_as_unknown() {
        let data: ErrorData =
            serde_json::from_value(json!({"kind": "planReplaced", "detail": {"planId": "p"}}))
                .unwrap();
        assert_eq!(data.kind, ErrorKind::Unknown);
    }

    #[test]
    fn wisp_errors_carry_their_kind_in_data() {
        let error = ErrorObject::wisp(ErrorKind::NotInitialized, "initialize first");
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"code": -32000, "message": "initialize first", "data": {"kind": "notInitialized"}})
        );
        assert_eq!(
            error.wisp_data(),
            Some(ErrorData {
                kind: ErrorKind::NotInitialized,
                detail: None
            })
        );
        assert_eq!(ErrorObject::new(INTERNAL_ERROR, "x").wisp_data(), None);
        assert_eq!(ErrorObject::new(WISP_ERROR, "no data").wisp_data(), None);
    }

    #[test]
    fn incompatible_protocol_has_its_frozen_shape() {
        let detail = IncompatibleProtocolDetail {
            requested: ProtocolRange { min: 2, max: 3 },
            supported: ProtocolRange { min: 1, max: 1 },
            wispd: "0.1.0".to_owned(),
        };
        let error = ErrorObject::incompatible_protocol(&detail);
        assert_eq!(
            serde_json::to_value(&error).unwrap()["data"],
            json!({
                "kind": "incompatibleProtocol",
                "detail": {
                    "requested": {"min": 2, "max": 3},
                    "supported": {"min": 1, "max": 1},
                    "wispd": "0.1.0"
                }
            })
        );
        let data = error.wisp_data().unwrap();
        assert_eq!(
            serde_json::from_value::<IncompatibleProtocolDetail>(data.detail.unwrap()).unwrap(),
            detail
        );
        assert!(error.message.contains("0.1.0"), "{}", error.message);
    }
}
