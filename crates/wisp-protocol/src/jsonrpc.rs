//! The JSON-RPC 2.0 envelope, in both directions.
//!
//! [`Message::from_frame`] parses one frame. Traffic that is not a JSON-RPC 2.0 message gets one
//! of the standard codes: [`PARSE_ERROR`] for invalid UTF-8 or JSON, and [`INVALID_REQUEST`] for
//! valid JSON of the wrong shape, including batches, which the protocol does not use.
//! [`Request::params`] maps params that do not decode to [`INVALID_PARAMS`].
//!
//! Wisp's own errors use [`WISP_ERROR`] with `data: {kind, detail?}`; see
//! [`ErrorObject::wisp`].

use std::error::Error;
use std::fmt;

use serde::de::DeserializeOwned;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value};
use ts_rs::TS;

use crate::methods::{NotificationMethod, RequestMethod};

/// Invalid JSON, or bytes that are not UTF-8. The connection stays open.
pub const PARSE_ERROR: i64 = -32700;
/// Valid JSON that is not a JSON-RPC 2.0 request, notification, or response.
pub const INVALID_REQUEST: i64 = -32600;
/// A request for a method the receiver does not have.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// Params that do not decode as the method's params type.
pub const INVALID_PARAMS: i64 = -32602;
/// A failure inside the receiver.
pub const INTERNAL_ERROR: i64 = -32603;
/// A wisp error. Its `data` is an [`ErrorData`](crate::ErrorData), and receivers match on
/// `data.kind`, never on `message`.
pub const WISP_ERROR: i64 = -32000;
/// A request cancelled by `$/cancelRequest`.
pub const REQUEST_CANCELLED: i64 = -32800;

pub(crate) const CODES: [(&str, i64); 7] = [
    ("ParseError", PARSE_ERROR),
    ("InvalidRequest", INVALID_REQUEST),
    ("MethodNotFound", METHOD_NOT_FOUND),
    ("InvalidParams", INVALID_PARAMS),
    ("InternalError", INTERNAL_ERROR),
    ("WispError", WISP_ERROR),
    ("RequestCancelled", REQUEST_CANCELLED),
];

const MAX_QUOTED_BYTES: usize = 1024;

/// A request id, chosen by the sender: an integer or a string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum RequestId {
    /// An integer id.
    Number(i64),
    /// A string id.
    String(String),
}

impl From<i64> for RequestId {
    fn from(id: i64) -> Self {
        Self::Number(id)
    }
}

impl From<String> for RequestId {
    fn from(id: String) -> Self {
        Self::String(id)
    }
}

impl From<&str> for RequestId {
    fn from(id: &str) -> Self {
        Self::String(id.to_owned())
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(id) => write!(f, "{id}"),
            Self::String(id) => write!(f, "{id:?}"),
        }
    }
}

/// A call that gets exactly one [`Response`], even when it is cancelled.
///
/// `P` is the params type: a method's `Params` when sending, and [`Value`] after parsing.
#[derive(Clone, Debug, PartialEq)]
pub struct Request<P = Value> {
    /// Matches the response to the request.
    pub id: RequestId,
    /// The method name.
    pub method: String,
    /// The params. Absent and `null` params are both `None`.
    pub params: Option<P>,
}

impl<P> Request<P> {
    /// A request for method `M`.
    pub fn new<M: RequestMethod<Params = P>>(id: impl Into<RequestId>, params: P) -> Self {
        Self {
            id: id.into(),
            method: M::NAME.to_owned(),
            params: Some(params),
        }
    }
}

impl Request {
    /// Decodes the params as `T`. Absent params decode as `{}`.
    ///
    /// # Errors
    ///
    /// An [`INVALID_PARAMS`] error, ready to send, when the params do not decode.
    pub fn params<T: DeserializeOwned>(&self) -> Result<T, ErrorObject> {
        decode_params(self.params.as_ref())
    }
}

/// A message that gets no response.
///
/// `P` is the params type: a method's `Params` when sending, and [`Value`] after parsing.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification<P = Value> {
    /// The method name.
    pub method: String,
    /// The params. Absent and `null` params are both `None`.
    pub params: Option<P>,
}

impl<P> Notification<P> {
    /// A notification for method `N`.
    pub fn new<N: NotificationMethod<Params = P>>(params: P) -> Self {
        Self {
            method: N::NAME.to_owned(),
            params: Some(params),
        }
    }
}

impl Notification {
    /// Decodes the params as `T`. Absent params decode as `{}`.
    ///
    /// # Errors
    ///
    /// An [`INVALID_PARAMS`] error when the params do not decode.
    pub fn params<T: DeserializeOwned>(&self) -> Result<T, ErrorObject> {
        decode_params(self.params.as_ref())
    }
}

fn decode_params<T: DeserializeOwned>(params: Option<&Value>) -> Result<T, ErrorObject> {
    let empty = Value::Object(Map::new());
    T::deserialize(params.unwrap_or(&empty)).map_err(ErrorObject::invalid_params)
}

/// The answer to a request: a result or an error.
///
/// `R` is the result type: a method's `Result` when sending, and [`Value`] after parsing.
#[derive(Clone, Debug, PartialEq)]
pub struct Response<R = Value> {
    /// The request's id. `None` is sent as `null`, for a request whose id could not be read.
    pub id: Option<RequestId>,
    /// The result, or the error.
    pub result: Result<R, ErrorObject>,
}

impl<R> Response<R> {
    /// A successful response.
    pub fn success(id: RequestId, result: R) -> Self {
        Self {
            id: Some(id),
            result: Ok(result),
        }
    }
}

impl Response {
    /// An error response.
    #[must_use]
    pub fn error(id: Option<RequestId>, error: ErrorObject) -> Self {
        Self {
            id,
            result: Err(error),
        }
    }

    /// Decodes the result as `T`.
    ///
    /// # Errors
    ///
    /// The response's error, or an [`INTERNAL_ERROR`] when the result does not decode as `T`.
    pub fn into_result<T: DeserializeOwned>(self) -> Result<T, ErrorObject> {
        serde_json::from_value(self.result?).map_err(|error| {
            ErrorObject::new(INTERNAL_ERROR, truncate(format!("Invalid result: {error}")))
        })
    }
}

/// Any message, as parsed from a frame.
#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    /// A request, which has an id.
    Request(Request),
    /// A notification, which has no id.
    Notification(Notification),
    /// A response to one of our requests.
    Response(Response),
}

impl Message {
    /// Parses one frame. Keys the envelope does not use are ignored.
    ///
    /// # Errors
    ///
    /// A [`MalformedMessage`] when the frame is not UTF-8 JSON ([`PARSE_ERROR`]) or not a
    /// JSON-RPC 2.0 message ([`INVALID_REQUEST`]). Answer it with
    /// [`MalformedMessage::into_response`]; the connection stays open.
    pub fn from_frame(frame: &[u8]) -> Result<Self, MalformedMessage> {
        let value = serde_json::from_slice(frame).map_err(|error| MalformedMessage {
            id: None,
            error: ErrorObject::new(PARSE_ERROR, format!("Parse error: {error}")),
        })?;
        Self::from_value(value)
    }

    fn from_value(value: Value) -> Result<Self, MalformedMessage> {
        let Value::Object(mut message) = value else {
            return Err(MalformedMessage::invalid(
                None,
                "a message must be a JSON object; batches are not supported",
            ));
        };
        let id = match message.remove("id") {
            None => IdMember::Absent,
            Some(Value::Null) => IdMember::Null,
            Some(id) => match RequestId::deserialize(id) {
                Ok(id) => IdMember::Id(id),
                Err(_) => {
                    return Err(MalformedMessage::invalid(
                        None,
                        "id must be a string or an integer",
                    ));
                }
            },
        };
        // Ids belong to the side that sent the request, so an error reply may echo the id only
        // of a request. Echoing a malformed response's id would answer the peer's own request.
        let known_id = match &id {
            IdMember::Id(id) if message.contains_key("method") => Some(id.clone()),
            IdMember::Id(_) | IdMember::Absent | IdMember::Null => None,
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(MalformedMessage::invalid(
                known_id,
                "jsonrpc must be \"2.0\"",
            ));
        }

        if let Some(method) = message.remove("method") {
            let Value::String(method) = method else {
                return Err(MalformedMessage::invalid(
                    known_id,
                    "method must be a string",
                ));
            };
            let params = match message.remove("params") {
                None | Some(Value::Null) => None,
                Some(params @ (Value::Object(_) | Value::Array(_))) => Some(params),
                Some(_) => {
                    return Err(MalformedMessage::invalid(
                        known_id,
                        "params must be an object or an array",
                    ));
                }
            };
            return match id {
                IdMember::Id(id) => Ok(Self::Request(Request { id, method, params })),
                IdMember::Absent => Ok(Self::Notification(Notification { method, params })),
                IdMember::Null => Err(MalformedMessage::invalid(
                    None,
                    "a request's id must not be null",
                )),
            };
        }

        match (message.remove("result"), message.remove("error"), id) {
            (Some(result), None, IdMember::Id(id)) => {
                Ok(Self::Response(Response::success(id, result)))
            }
            (None, Some(error), id) => match ErrorObject::deserialize(error) {
                Ok(error) => Ok(Self::Response(Response::error(known_id_of(id), error))),
                Err(_) => Err(MalformedMessage::invalid(
                    known_id,
                    "error must be an object with an integer code and a string message",
                )),
            },
            _ => Err(MalformedMessage::invalid(
                known_id,
                "a message needs a method, or an id with exactly one of result and error",
            )),
        }
    }
}

enum IdMember {
    Absent,
    Null,
    Id(RequestId),
}

fn known_id_of(id: IdMember) -> Option<RequestId> {
    match id {
        IdMember::Id(id) => Some(id),
        IdMember::Absent | IdMember::Null => None,
    }
}

impl<P: Serialize> Serialize for Request<P> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut request = serializer.serialize_struct("Request", 4)?;
        request.serialize_field("jsonrpc", "2.0")?;
        request.serialize_field("id", &self.id)?;
        request.serialize_field("method", &self.method)?;
        match &self.params {
            Some(params) => request.serialize_field("params", params)?,
            None => request.skip_field("params")?,
        }
        request.end()
    }
}

impl<P: Serialize> Serialize for Notification<P> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut notification = serializer.serialize_struct("Notification", 3)?;
        notification.serialize_field("jsonrpc", "2.0")?;
        notification.serialize_field("method", &self.method)?;
        match &self.params {
            Some(params) => notification.serialize_field("params", params)?,
            None => notification.skip_field("params")?,
        }
        notification.end()
    }
}

impl<R: Serialize> Serialize for Response<R> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut response = serializer.serialize_struct("Response", 3)?;
        response.serialize_field("jsonrpc", "2.0")?;
        response.serialize_field("id", &self.id)?;
        match &self.result {
            Ok(result) => response.serialize_field("result", result)?,
            Err(error) => response.serialize_field("error", error)?,
        }
        response.end()
    }
}

impl Serialize for Message {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Request(request) => request.serialize(serializer),
            Self::Notification(notification) => notification.serialize(serializer),
            Self::Response(response) => response.serialize(serializer),
        }
    }
}

/// A JSON-RPC error object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorObject {
    /// One of the codes in this module.
    pub code: i64,
    /// A description for people. Receivers never match on it.
    pub message: String,
    /// More about the error. For [`WISP_ERROR`], an [`ErrorData`](crate::ErrorData).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ErrorObject {
    /// An error with no data.
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// [`METHOD_NOT_FOUND`] for `method`.
    #[must_use]
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            METHOD_NOT_FOUND,
            truncate(format!("Method not found: {method}")),
        )
    }

    /// [`INVALID_PARAMS`], with the reason the params did not decode.
    pub fn invalid_params(reason: impl fmt::Display) -> Self {
        Self::new(
            INVALID_PARAMS,
            truncate(format!("Invalid params: {reason}")),
        )
    }

    /// [`INTERNAL_ERROR`], with a description.
    pub fn internal_error(description: impl fmt::Display) -> Self {
        Self::new(
            INTERNAL_ERROR,
            truncate(format!("Internal error: {description}")),
        )
    }

    /// [`REQUEST_CANCELLED`], the answer to a request that `$/cancelRequest` cancelled.
    #[must_use]
    pub fn request_cancelled() -> Self {
        Self::new(REQUEST_CANCELLED, "Request cancelled")
    }
}

impl fmt::Display for ErrorObject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl Error for ErrorObject {}

// Messages that quote the peer's input are cut, so an error response never grows with that
// input and cannot exceed the frame limit.
fn truncate(mut message: String) -> String {
    if message.len() > MAX_QUOTED_BYTES {
        let mut end = MAX_QUOTED_BYTES;
        while !message.is_char_boundary(end) {
            end -= 1;
        }
        message.truncate(end);
        message.push_str("...");
    }
    message
}

/// A frame that is not a JSON-RPC 2.0 message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MalformedMessage {
    /// The id, when the frame was a request whose id could still be read.
    pub id: Option<RequestId>,
    /// [`PARSE_ERROR`] or [`INVALID_REQUEST`].
    pub error: ErrorObject,
}

impl MalformedMessage {
    fn invalid(id: Option<RequestId>, reason: &str) -> Self {
        Self {
            id,
            error: ErrorObject::new(INVALID_REQUEST, format!("Invalid request: {reason}")),
        }
    }

    /// The error response to send back.
    #[must_use]
    pub fn into_response(self) -> Response {
        Response::error(self.id, self.error)
    }
}

impl fmt::Display for MalformedMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl Error for MalformedMessage {}

/// Params of `$/cancelRequest`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CancelRequestParams {
    /// The id of the request to cancel.
    pub id: RequestId,
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        CancelRequestParams, ErrorObject, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST,
        MAX_QUOTED_BYTES, Message, Notification, PARSE_ERROR, Request, RequestId, Response,
    };
    use crate::methods::{CancelRequest, HostHealth, ProjectCreate};
    use crate::{HostHealthParams, ProjectCreateParams};

    fn parse(json: &str) -> Result<Message, super::MalformedMessage> {
        Message::from_frame(json.as_bytes())
    }

    fn invalid(json: &str) -> (Option<RequestId>, i64) {
        let malformed = parse(json).expect_err(json);
        (malformed.id, malformed.error.code)
    }

    #[test]
    fn requests_notifications_and_responses_are_told_apart() {
        let Message::Request(request) =
            parse(r#"{"jsonrpc":"2.0","id":7,"method":"host/health","params":{}}"#).unwrap()
        else {
            panic!("expected a request");
        };
        assert_eq!(request.id, RequestId::Number(7));
        assert_eq!(request.method, "host/health");
        assert_eq!(request.params, Some(json!({})));

        let Message::Notification(notification) =
            parse(r#"{"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":"a"}}"#).unwrap()
        else {
            panic!("expected a notification");
        };
        assert_eq!(
            notification.params::<CancelRequestParams>().unwrap().id,
            RequestId::from("a")
        );

        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","id":"x","result":null}"#).unwrap(),
            Message::Response(Response::success(RequestId::from("x"), Value::Null))
        );
        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"Parse error"}}"#)
                .unwrap(),
            Message::Response(Response::error(
                None,
                ErrorObject::new(PARSE_ERROR, "Parse error")
            ))
        );
    }

    #[test]
    fn malformed_json_and_invalid_utf8_are_parse_errors() {
        for frame in [
            &b"{"[..],
            b"not json",
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"a\"} trailing",
            b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"\xff\xfe\"}",
            b"\xc3\x28",
        ] {
            let malformed = Message::from_frame(frame).unwrap_err();
            assert_eq!(malformed.id, None);
            assert_eq!(malformed.error.code, PARSE_ERROR);
            let response = serde_json::to_value(malformed.into_response()).unwrap();
            assert_eq!(response["id"], Value::Null);
            assert_eq!(response["error"]["code"], PARSE_ERROR);
        }
    }

    #[test]
    fn wrong_shapes_are_invalid_requests_with_a_request_id_when_readable() {
        assert_eq!(invalid("[]"), (None, INVALID_REQUEST));
        assert_eq!(
            invalid(r#"[{"jsonrpc":"2.0","method":"a"}]"#),
            (None, INVALID_REQUEST)
        );
        assert_eq!(invalid("42"), (None, INVALID_REQUEST));
        assert_eq!(
            invalid(r#"{"id":1,"method":"a"}"#),
            (Some(1.into()), INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"1.0","id":1,"method":"a"}"#),
            (Some(1.into()), INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"2.0","id":2,"method":7}"#),
            (Some(2.into()), INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"2.0","id":3,"method":"a","params":5}"#),
            (Some(3.into()), INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"2.0","id":1.5,"method":"a"}"#),
            (None, INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"2.0","id":{},"method":"a"}"#),
            (None, INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"2.0","id":null,"method":"a"}"#),
            (None, INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"2.0","id":4}"#),
            (None, INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"2.0","id":5,"result":1,"error":{"code":1,"message":""}}"#),
            (None, INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"2.0","result":1}"#),
            (None, INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"2.0","id":6,"error":{"code":"x","message":""}}"#),
            (None, INVALID_REQUEST)
        );
        assert_eq!(
            invalid(r#"{"jsonrpc":"1.0","id":7,"result":1}"#),
            (None, INVALID_REQUEST)
        );
    }

    #[test]
    fn a_malformed_response_is_not_answered_with_the_peers_own_id() {
        let malformed =
            parse(r#"{"jsonrpc":"2.0","id":6,"error":{"code":"x","message":""}}"#).unwrap_err();
        let reply = serde_json::to_value(malformed.into_response()).unwrap();
        assert_eq!(reply["id"], Value::Null);
        assert_eq!(reply["error"]["code"], INVALID_REQUEST);
    }

    #[test]
    fn unknown_envelope_keys_and_null_params_are_ignored() {
        let Message::Request(request) =
            parse(r#"{"jsonrpc":"2.0","id":1,"method":"host/health","params":null,"trace":"x"}"#)
                .unwrap()
        else {
            panic!("expected a request");
        };
        assert_eq!(request.params, None);
        assert_eq!(
            request.params::<HostHealthParams>(),
            Ok(HostHealthParams {})
        );
    }

    #[test]
    fn an_error_response_without_an_id_is_accepted() {
        assert_eq!(
            parse(r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"}}"#).unwrap(),
            Message::Response(Response::error(
                None,
                ErrorObject::new(PARSE_ERROR, "Parse error")
            ))
        );
    }

    #[test]
    fn params_that_do_not_decode_are_invalid_params() {
        let request = Request {
            id: 1.into(),
            method: "project/create".to_owned(),
            params: Some(json!({"id": "nope", "name": "a", "repoPath": "/a"})),
        };
        let error = request.params::<ProjectCreateParams>().unwrap_err();
        assert_eq!(error.code, INVALID_PARAMS);
        assert!(error.message.contains("UUIDv7"), "{}", error.message);

        let missing = Request {
            params: None,
            ..request
        };
        assert_eq!(
            missing.params::<ProjectCreateParams>().unwrap_err().code,
            INVALID_PARAMS
        );
    }

    #[test]
    fn error_messages_quoting_input_stay_short() {
        let huge = "x".repeat(100_000);
        assert!(ErrorObject::method_not_found(&huge).message.len() <= MAX_QUOTED_BYTES + 3);
        let request = Request {
            id: 1.into(),
            method: "project/create".to_owned(),
            params: Some(json!({"id": huge, "name": 1, "repoPath": "/"})),
        };
        let error = request.params::<ProjectCreateParams>().unwrap_err();
        assert!(
            error.message.len() <= MAX_QUOTED_BYTES + 3,
            "{}",
            error.message.len()
        );
        let multibyte = "\u{e9}".repeat(2000);
        assert!(
            ErrorObject::internal_error(multibyte)
                .message
                .ends_with("...")
        );
    }

    #[test]
    fn typed_messages_serialize_as_json_rpc_2() {
        let request = Request::new::<HostHealth>(1, HostHealthParams {});
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"jsonrpc": "2.0", "id": 1, "method": "host/health", "params": {}})
        );
        let cancel = Notification::new::<CancelRequest>(CancelRequestParams { id: 1.into() });
        assert_eq!(
            serde_json::to_value(&cancel).unwrap(),
            json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 1}})
        );
        let error = Response::error(Some("a".into()), ErrorObject::request_cancelled());
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"jsonrpc": "2.0", "id": "a", "error": {"code": -32800, "message": "Request cancelled"}})
        );
        let bare = Notification {
            method: "a".to_owned(),
            params: None::<Value>,
        };
        assert_eq!(
            serde_json::to_value(&bare).unwrap(),
            json!({"jsonrpc": "2.0", "method": "a"})
        );
    }

    #[test]
    fn typed_messages_round_trip_through_a_frame() {
        let params = ProjectCreateParams {
            id: "01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e7f".parse().unwrap(),
            name: "wisp".to_owned(),
            repo_path: "/Users/ryan/wisp".to_owned(),
        };
        let frame =
            serde_json::to_vec(&Request::new::<ProjectCreate>("r1", params.clone())).unwrap();
        let Message::Request(request) = Message::from_frame(&frame).unwrap() else {
            panic!("expected a request");
        };
        assert_eq!(request.params::<ProjectCreateParams>(), Ok(params));
    }

    #[test]
    fn results_decode_or_report_why_not() {
        let ok = Response::success(
            1.into(),
            json!({"subscription": "01997c3a-5b2c-7d4e-9f10-2a3b4c5d6e7f"}),
        );
        assert!(ok.into_result::<crate::EventsSubscribeResult>().is_ok());
        let wrong = Response::success(1.into(), json!({"subscription": 5}));
        assert_eq!(
            wrong
                .into_result::<crate::EventsSubscribeResult>()
                .unwrap_err()
                .code,
            INTERNAL_ERROR
        );
        let failed = Response::error(Some(1.into()), ErrorObject::request_cancelled());
        assert_eq!(
            failed.into_result::<crate::EventsSubscribeResult>(),
            Err(ErrorObject::request_cancelled())
        );
    }
}
