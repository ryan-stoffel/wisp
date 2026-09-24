// Checks samples/ against the rules in the crate docs, under "Changing the protocol".

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use ts_rs::{Config, TS};

use crate::jsonrpc::{
    CODES, ErrorObject, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, Message, Notification,
    PARSE_ERROR, Request, RequestId, Response, WISP_ERROR,
};
use crate::methods::{self, NotificationMethod, RequestMethod};
use crate::{ErrorKind, IncompatibleProtocolDetail, PROTOCOL_VERSION, StoreState, WispEvent};

#[test]
fn every_sample_decodes_and_exact_samples_encode_back_unchanged() {
    let mut coverage = Coverage::default();
    for (path, exact) in sample_files() {
        check_file(&path, exact, &mut coverage);
    }
    coverage.assert_complete();
}

fn sample_files() -> Vec<(PathBuf, bool)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("samples");
    let mut versions = BTreeSet::new();
    for entry in fs::read_dir(&root).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        let version: u32 = name
            .strip_prefix('v')
            .and_then(|number| number.parse().ok())
            .unwrap_or_else(|| panic!("samples/{name} should be named v<protocol version>"));
        assert!(
            (1..=PROTOCOL_VERSION).contains(&version),
            "samples/{name} is for a protocol version this crate does not speak"
        );
        versions.insert(version);
    }
    assert!(versions.contains(&1), "samples/v1 is missing");
    let mut files = Vec::new();
    for version in versions {
        let directory = root.join(format!("v{version}"));
        files.extend(json_files(&directory).into_iter().map(|path| (path, true)));
        files.extend(
            json_files(&directory.join("lenient"))
                .into_iter()
                .map(|path| (path, false)),
        );
    }
    files
}

fn json_files(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    files
}

enum Expect {
    Result(&'static str),
    MethodNotFound,
    InvalidParams,
}

fn check_file(path: &Path, exact: bool, coverage: &mut Coverage) {
    let name = path
        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
        .unwrap()
        .display()
        .to_string();
    let text = fs::read_to_string(path).unwrap();
    let Ok(Value::Array(messages)) = serde_json::from_str(&text) else {
        panic!("{name} should be a JSON array of messages");
    };
    let mut transcript = Transcript {
        exact,
        at: String::new(),
        pending: HashMap::new(),
        coverage,
    };
    for (index, original) in messages.into_iter().enumerate() {
        transcript.at = format!("{name}, message {index}");
        transcript.check(&original);
    }
    if exact {
        assert!(
            transcript.pending.is_empty(),
            "{name}: a request has no response"
        );
    }
}

struct Transcript<'a> {
    exact: bool,
    at: String,
    pending: HashMap<RequestId, Expect>,
    coverage: &'a mut Coverage,
}

impl Transcript<'_> {
    fn check(&mut self, original: &Value) {
        let at = &self.at;
        let frame = serde_json::to_vec(original).unwrap();
        let message = Message::from_frame(&frame).unwrap_or_else(|error| panic!("{at}: {error}"));
        if self.exact {
            assert_eq!(
                &serde_json::to_value(&message).unwrap(),
                original,
                "{at}: the envelope does not encode back unchanged"
            );
        }
        match message {
            Message::Request(request) => self.request(request),
            Message::Notification(notification) => self.notification(&notification),
            Message::Response(response) => self.response(response),
        }
    }

    fn request(&mut self, request: Request) {
        let at = &self.at;
        let expect = match decode(&request.method, Part::Params(request.params.as_ref())) {
            None => Expect::MethodNotFound,
            Some((_, Err(_))) => Expect::InvalidParams,
            Some((method, Ok(encoded))) => {
                if self.exact {
                    assert_eq!(
                        Some(&encoded),
                        request.params.as_ref(),
                        "{at}: the params do not encode back unchanged"
                    );
                    self.coverage.requests.insert(method);
                }
                Expect::Result(method)
            }
        };
        self.pending.insert(request.id, expect);
    }

    fn notification(&mut self, notification: &Notification) {
        let at = &self.at;
        let params = notification.params.as_ref();
        match decode(&notification.method, Part::Notification(params)) {
            None => assert!(!self.exact, "{at}: unknown notification in an exact sample"),
            Some((_, Err(error))) => panic!("{at}: {error}"),
            Some((method, Ok(encoded))) => {
                if self.exact {
                    assert_eq!(Some(&encoded), params, "{at}: the params changed");
                    self.coverage.notifications.insert(method);
                    if let Some(kind) = encoded["event"]["kind"].as_str() {
                        self.coverage.event_kinds.insert(kind.to_owned());
                    }
                }
            }
        }
    }

    fn response(&mut self, response: Response) {
        let at = &self.at;
        let expect = response.id.as_ref().map(|id| {
            self.pending
                .remove(id)
                .unwrap_or_else(|| panic!("{at}: it answers no earlier request"))
        });
        match (response.result, expect) {
            (Ok(result), Some(Expect::Result(method))) => {
                let (_, encoded) = decode(method, Part::Result(&result)).unwrap();
                let encoded = encoded.unwrap_or_else(|error| panic!("{at}: {error}"));
                if self.exact {
                    assert_eq!(encoded, result, "{at}: the result changed");
                    self.coverage.results.insert(method);
                    if let Some(store) = result["store"].as_str() {
                        self.coverage.store_states.insert(store.to_owned());
                    }
                }
            }
            (Ok(_), _) => panic!("{at}: this request can only get an error"),
            (Err(error), expect) => {
                let expected = match expect {
                    None => vec![PARSE_ERROR, INVALID_REQUEST],
                    Some(Expect::MethodNotFound) => vec![METHOD_NOT_FOUND],
                    Some(Expect::InvalidParams) => vec![INVALID_PARAMS],
                    Some(Expect::Result(_)) => CODES.map(|(_, code)| code).to_vec(),
                };
                assert!(expected.contains(&error.code), "{at}: unexpected code");
                self.error(&error);
            }
        }
    }

    fn error(&mut self, error: &ErrorObject) {
        let at = &self.at;
        if self.exact {
            self.coverage.codes.insert(error.code);
        }
        if error.code != WISP_ERROR {
            return;
        }
        let data = error
            .wisp_data()
            .unwrap_or_else(|| panic!("{at}: data should be {{kind, detail?}}"));
        if data.kind == ErrorKind::IncompatibleProtocol {
            let detail = data.detail.clone().unwrap_or_default();
            let decoded: IncompatibleProtocolDetail = serde_json::from_value(detail.clone())
                .unwrap_or_else(|error| panic!("{at}: {error}"));
            if self.exact {
                assert_eq!(serde_json::to_value(decoded).unwrap(), detail, "{at}");
            }
        }
        if self.exact {
            let encoded = serde_json::to_value(&data).unwrap();
            assert_eq!(
                Some(&encoded),
                error.data.as_ref(),
                "{at}: the data changed"
            );
            let kind = encoded["kind"].as_str().unwrap().to_owned();
            self.coverage.error_kinds.insert(kind);
        }
    }
}

#[derive(Clone, Copy)]
enum Part<'a> {
    Params(Option<&'a Value>),
    Result(&'a Value),
    Notification(Option<&'a Value>),
}

// Decodes `part` into the typed form for `method` and encodes it again. `None` means the method
// is not in the table.
fn decode(method: &str, part: Part<'_>) -> Option<(&'static str, Result<Value, String>)> {
    let mut decode = Decode {
        method,
        part,
        outcome: None,
    };
    methods::visit(&mut decode);
    decode.outcome
}

struct Decode<'a> {
    method: &'a str,
    part: Part<'a>,
    outcome: Option<(&'static str, Result<Value, String>)>,
}

impl methods::Visitor for Decode<'_> {
    fn request<M: RequestMethod>(&mut self, _: &[&str]) {
        if M::NAME == self.method {
            let encoded = match self.part {
                Part::Params(params) => reencode::<M::Params>(params),
                Part::Result(result) => reencode::<M::Result>(Some(result)),
                Part::Notification(_) => return,
            };
            self.outcome = Some((M::NAME, encoded));
        }
    }

    fn notification<N: NotificationMethod>(&mut self, _: &[&str]) {
        if let (true, Part::Notification(params)) = (N::NAME == self.method, self.part) {
            self.outcome = Some((N::NAME, reencode::<N::Params>(params)));
        }
    }
}

// Absent params decode as `{}`, as in `Request::params`.
fn reencode<T: Serialize + DeserializeOwned>(value: Option<&Value>) -> Result<Value, String> {
    let empty = Value::Object(Map::new());
    let typed = T::deserialize(value.unwrap_or(&empty)).map_err(|error| error.to_string())?;
    serde_json::to_value(typed).map_err(|error| error.to_string())
}

#[derive(Default)]
struct Coverage {
    requests: BTreeSet<&'static str>,
    results: BTreeSet<&'static str>,
    notifications: BTreeSet<&'static str>,
    codes: BTreeSet<i64>,
    error_kinds: BTreeSet<String>,
    event_kinds: BTreeSet<String>,
    store_states: BTreeSet<String>,
}

impl Coverage {
    fn assert_complete(&self) {
        let mut table = MethodNames::default();
        methods::visit(&mut table);
        let config = Config::default();
        let mut event_kinds = string_literals(&WispEvent::decl(&config));
        event_kinds.remove("kind");

        let missing = "the exact samples should use every";
        assert_eq!(self.requests, table.requests, "{missing} request");
        assert_eq!(self.results, table.requests, "{missing} request's result");
        assert_eq!(
            self.notifications, table.notifications,
            "{missing} notification"
        );
        assert_eq!(
            self.codes,
            CODES.map(|(_, code)| code).into(),
            "{missing} error code"
        );
        assert_eq!(
            self.error_kinds,
            string_literals(&ErrorKind::decl(&config)),
            "{missing} error kind"
        );
        assert_eq!(self.event_kinds, event_kinds, "{missing} event kind");
        assert_eq!(
            self.store_states,
            string_literals(&StoreState::decl(&config)),
            "{missing} store state"
        );
    }
}

#[derive(Default)]
struct MethodNames {
    requests: BTreeSet<&'static str>,
    notifications: BTreeSet<&'static str>,
}

impl methods::Visitor for MethodNames {
    fn request<M: RequestMethod>(&mut self, _: &[&str]) {
        self.requests.insert(M::NAME);
    }

    fn notification<N: NotificationMethod>(&mut self, _: &[&str]) {
        self.notifications.insert(N::NAME);
    }
}

// The string literals of a generated declaration, outside its comments. For a union, these are
// its values, without the fallback variants that ts-rs skips.
fn string_literals(declaration: &str) -> BTreeSet<String> {
    let mut literals = BTreeSet::new();
    let mut rest = declaration;
    while let Some(start) = rest.find(['"', '/']) {
        let tail = &rest[start..];
        if let Some(comment) = tail.strip_prefix("/*") {
            rest = comment.split_once("*/").map_or("", |(_, after)| after);
        } else if let Some(string) = tail.strip_prefix('"') {
            let (literal, after) = string.split_once('"').unwrap();
            literals.insert(literal.to_owned());
            rest = after;
        } else {
            rest = &tail[1..];
        }
    }
    literals
}
