//! JSON-RPC 2.0 framing (ARCH-025): one object per line (`\n`), UTF-8, at most
//! [`MAX_LINE_BYTES`] per line, no batches. JSON-RPC errors are used only for protocol faults
//! (QA-010); every method's `result` is an `Outcome`.
//!
//! The cap holds both ways: the core checks every line it writes with [`fits_line`] before it
//! is sent, since a client reading with the same cap closes the connection on a longer one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Longest line, in bytes, without the `\n`.
pub const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;

/// The room kept in a line for the response envelope around a `result`
/// (`{"jsonrpc":"2.0","id":…,"result":…}`): enough for any numeric id and for string ids of
/// a few thousand bytes. A longer id is still safe: [`encode_response_capped`] catches it.
pub const RESPONSE_ENVELOPE_RESERVE: usize = 4096;

/// Whether `encoded`, one line without its `\n`, is within [`MAX_LINE_BYTES`].
pub fn fits_line(encoded: &str) -> bool {
    encoded.len() <= MAX_LINE_BYTES
}

/// Whether `encoded_result`, the JSON of a response's `result`, leaves room in one line for
/// its envelope ([`RESPONSE_ENVELOPE_RESERVE`]).
pub fn fits_response_result(encoded_result: &str) -> bool {
    encoded_result
        .len()
        .checked_add(RESPONSE_ENVELOPE_RESERVE)
        .is_some_and(|n| n <= MAX_LINE_BYTES)
}

/// JSON-RPC protocol-fault codes (QA-010).
pub mod rpc_code {
    /// Not JSON (the id is `null`).
    pub const PARSE_ERROR: i64 = -32700;
    /// Not a request object; also a batch, and a second `core/hello`.
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Bad params; also an unknown `core/hello` field.
    pub const INVALID_PARAMS: i64 = -32602;
    pub const HANDSHAKE_REQUIRED: i64 = -32001;
    /// The line is longer than `MAX_LINE_BYTES`; the connection is then closed.
    pub const LINE_TOO_LONG: i64 = -32002;
    /// The core's response would have been longer than `MAX_LINE_BYTES`, so it was not sent.
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// Method names (ARCH-031).
pub mod method {
    pub const HELLO: &str = "core/hello";
    pub const DRAIN: &str = "core/drain";
    pub const TOOLS_LIST: &str = "tools/list";
    pub const TOOLS_CALL: &str = "tools/call";
    /// Namespace of the event methods (map row 19), a reserved prefix like [`APP_PREFIX`].
    pub const PREFIX: &str = "lil/";
    pub const SUBSCRIBE: &str = "lil/subscribe";
    pub const UNSUBSCRIBE: &str = "lil/unsubscribe";
    /// Notification, core → client.
    pub const EVENT: &str = "lil/event";
    /// Namespace of the app-only methods (verified `ui:*` callers).
    pub const APP_PREFIX: &str = "app/";
}

/// A request id: an integer or a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Num(i64),
    Str(String),
}

/// A request, or a notification when `id` is `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub id: Option<Id>,
    pub method: String,
    pub params: Option<Value>,
}

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// A response line. An error's id is `None` (`null`) when the request's id couldn't be read.
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Result { id: Id, result: Value },
    Error { id: Option<Id>, error: RpcError },
}

/// What a client reads from the core: a response or a notification.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerLine {
    Response(Response),
    Notification(Request),
}

/// Parses one line from a client. On failure, returns the id to echo (when it could be read)
/// and the protocol fault to send.
// `RpcError` carries a `serde_json::Value`, which `preserve_order` (ARCH-076) makes an ordered
// map; the error path runs once per bad line, so its size costs nothing worth a box.
#[allow(clippy::result_large_err)]
pub fn parse_request(line: &str) -> Result<Request, (Option<Id>, RpcError)> {
    if line.len() > MAX_LINE_BYTES {
        return Err((
            None,
            RpcError::new(rpc_code::LINE_TOO_LONG, "line too long"),
        ));
    }
    let value: Value = serde_json::from_str(line)
        .map_err(|_| (None, RpcError::new(rpc_code::PARSE_ERROR, "parse error")))?;
    let obj = match value {
        Value::Object(obj) => obj,
        Value::Array(_) => {
            return Err((None, invalid("batches aren't supported")));
        }
        _ => return Err((None, invalid("a request is a JSON object"))),
    };
    let id = match obj.get("id") {
        None => None,
        Some(raw) => Some(
            read_id(raw).ok_or_else(|| (None, invalid("the id must be an integer or a string")))?,
        ),
    };
    check_version(&obj).map_err(|e| (id.clone(), e))?;
    let method = match obj.get("method") {
        Some(Value::String(m)) => m.clone(),
        _ => return Err((id, invalid("the method must be a string"))),
    };
    let params = read_params(&obj).map_err(|e| (id.clone(), e))?;
    Ok(Request { id, method, params })
}

/// Parses one line from the core.
pub fn parse_server_line(line: &str) -> Result<ServerLine, RpcError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(RpcError::new(rpc_code::LINE_TOO_LONG, "line too long"));
    }
    let value: Value = serde_json::from_str(line)
        .map_err(|_| RpcError::new(rpc_code::PARSE_ERROR, "parse error"))?;
    let Value::Object(obj) = value else {
        return Err(invalid("a server line is a JSON object"));
    };
    check_version(&obj)?;
    let id = match obj.get("id") {
        None | Some(Value::Null) => None,
        Some(raw) => {
            Some(read_id(raw).ok_or_else(|| invalid("the id must be an integer or a string"))?)
        }
    };
    if let Some(m) = obj.get("method") {
        let Value::String(method) = m else {
            return Err(invalid("the method must be a string"));
        };
        if id.is_some() {
            return Err(invalid("the core sends no requests, only notifications"));
        }
        return Ok(ServerLine::Notification(Request {
            id: None,
            method: method.clone(),
            params: read_params(&obj)?,
        }));
    }
    match (obj.get("result"), obj.get("error")) {
        (Some(result), None) => {
            let id = id.ok_or_else(|| invalid("a result needs an id"))?;
            Ok(ServerLine::Response(Response::Result {
                id,
                result: result.clone(),
            }))
        }
        (None, Some(error)) => {
            let error: RpcError = serde_json::from_value(error.clone())
                .map_err(|_| invalid("malformed error object"))?;
            Ok(ServerLine::Response(Response::Error { id, error }))
        }
        _ => Err(invalid("a response has exactly one of result and error")),
    }
}

/// `{"jsonrpc":"2.0","id":…,"method":…,"params":…}` without a trailing newline. A `null`
/// `params` is left out.
pub fn encode_request(id: &Id, method: &str, params: &Value) -> String {
    let mut obj = envelope();
    obj.insert("id".into(), id_value(Some(id)));
    obj.insert("method".into(), Value::String(method.into()));
    if !params.is_null() {
        obj.insert("params".into(), params.clone());
    }
    Value::Object(obj).to_string()
}

/// `{"jsonrpc":"2.0","method":…,"params":…}` without a trailing newline. A `null` `params`
/// is left out.
pub fn encode_notification(method: &str, params: &Value) -> String {
    let mut obj = envelope();
    obj.insert("method".into(), Value::String(method.into()));
    if !params.is_null() {
        obj.insert("params".into(), params.clone());
    }
    Value::Object(obj).to_string()
}

/// A response line without a trailing newline.
pub fn encode_response(r: &Response) -> String {
    let mut obj = envelope();
    match r {
        Response::Result { id, result } => {
            obj.insert("id".into(), id_value(Some(id)));
            obj.insert("result".into(), result.clone());
        }
        Response::Error { id, error } => {
            obj.insert("id".into(), id_value(id.as_ref()));
            obj.insert(
                "error".into(),
                serde_json::to_value(error).expect("an RpcError serialises"),
            );
        }
    }
    Value::Object(obj).to_string()
}

/// [`encode_response`], within the cap: a line that [`fits_line`] is returned as it is, with
/// `None`. A longer one is never sent: it is replaced by a -32603 error for the same id (for
/// `null` when even that id does not fit), returned with the length it would have had.
pub fn encode_response_capped(r: &Response) -> (String, Option<usize>) {
    let line = encode_response(r);
    if fits_line(&line) {
        return (line, None);
    }
    let oversized = line.len();
    let id = match r {
        Response::Result { id, .. } => Some(id.clone()),
        Response::Error { id, .. } => id.clone(),
    };
    let error = RpcError::new(
        rpc_code::INTERNAL_ERROR,
        format!(
            "internal error: the response was too large to send ({oversized} bytes; a line holds at most {MAX_LINE_BYTES})"
        ),
    );
    let replacement = encode_response(&Response::Error {
        id,
        error: error.clone(),
    });
    if fits_line(&replacement) {
        return (replacement, Some(oversized));
    }
    (
        encode_response(&Response::Error { id: None, error }),
        Some(oversized),
    )
}

fn invalid(message: &str) -> RpcError {
    RpcError::new(
        rpc_code::INVALID_REQUEST,
        format!("invalid request: {message}"),
    )
}

fn envelope() -> Map<String, Value> {
    let mut obj = Map::new();
    obj.insert("jsonrpc".into(), Value::String("2.0".into()));
    obj
}

fn id_value(id: Option<&Id>) -> Value {
    match id {
        Some(Id::Num(n)) => Value::from(*n),
        Some(Id::Str(s)) => Value::String(s.clone()),
        None => Value::Null,
    }
}

/// An integer (fits `i64`) or a string; anything else (null, fraction, bool, …) is unusable.
fn read_id(raw: &Value) -> Option<Id> {
    match raw {
        Value::Number(n) => n.as_i64().map(Id::Num),
        Value::String(s) => Some(Id::Str(s.clone())),
        _ => None,
    }
}

fn check_version(obj: &Map<String, Value>) -> Result<(), RpcError> {
    match obj.get("jsonrpc") {
        Some(Value::String(v)) if v == "2.0" => Ok(()),
        _ => Err(invalid("\"jsonrpc\" must be \"2.0\"")),
    }
}

/// Params are an object or an array; absent or `null` means none.
fn read_params(obj: &Map<String, Value>) -> Result<Option<Value>, RpcError> {
    match obj.get("params") {
        None | Some(Value::Null) => Ok(None),
        Some(p @ (Value::Object(_) | Value::Array(_))) => Ok(Some(p.clone())),
        Some(_) => Err(invalid("params must be an object or an array")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test] // spec: ARCH-025, QA-010
    fn parse_request_classifies_lines() {
        // Not JSON: parse error, id null.
        let (id, err) = parse_request("{not json").unwrap_err();
        assert_eq!((id, err.code), (None, rpc_code::PARSE_ERROR));

        // A batch: invalid request.
        let (id, err) = parse_request(r#"[{"jsonrpc":"2.0","id":1,"method":"m"}]"#).unwrap_err();
        assert_eq!((id, err.code), (None, rpc_code::INVALID_REQUEST));

        // Not an object.
        let (id, err) = parse_request("42").unwrap_err();
        assert_eq!((id, err.code), (None, rpc_code::INVALID_REQUEST));

        // Missing or wrong jsonrpc: invalid request, id echoed.
        let (id, err) = parse_request(r#"{"id":7,"method":"tools/list"}"#).unwrap_err();
        assert_eq!(
            (id, err.code),
            (Some(Id::Num(7)), rpc_code::INVALID_REQUEST)
        );
        let (id, err) =
            parse_request(r#"{"jsonrpc":"1.0","id":"a","method":"tools/list"}"#).unwrap_err();
        assert_eq!(
            (id, err.code),
            (Some(Id::Str("a".into())), rpc_code::INVALID_REQUEST)
        );

        // Missing method, bad params.
        let (id, err) = parse_request(r#"{"jsonrpc":"2.0","id":8}"#).unwrap_err();
        assert_eq!(
            (id, err.code),
            (Some(Id::Num(8)), rpc_code::INVALID_REQUEST)
        );
        let (id, err) =
            parse_request(r#"{"jsonrpc":"2.0","id":9,"method":"m","params":3}"#).unwrap_err();
        assert_eq!(
            (id, err.code),
            (Some(Id::Num(9)), rpc_code::INVALID_REQUEST)
        );

        // An unusable id is not echoed.
        let (id, err) = parse_request(r#"{"jsonrpc":"2.0","id":1.5,"method":"m"}"#).unwrap_err();
        assert_eq!((id, err.code), (None, rpc_code::INVALID_REQUEST));

        // A request.
        let req = parse_request(
            r#"{"jsonrpc":"2.0","id":1,"method":"core/hello","params":{"protocol":"1.0"}}"#,
        )
        .unwrap();
        assert_eq!(
            req,
            Request {
                id: Some(Id::Num(1)),
                method: method::HELLO.into(),
                params: Some(json!({"protocol": "1.0"})),
            }
        );

        // A notification (no id).
        let note = parse_request(r#"{"jsonrpc":"2.0","method":"lil/event","params":{}}"#).unwrap();
        assert_eq!(note.id, None);
        assert_eq!(note.method, method::EVENT);

        // Round trip through the encoders.
        let line = encode_request(&Id::Str("x".into()), method::TOOLS_LIST, &json!({}));
        assert!(!line.contains('\n'));
        let back = parse_request(&line).unwrap();
        assert_eq!(back.id, Some(Id::Str("x".into())));
        assert_eq!(back.method, "tools/list");
        assert_eq!(back.params, Some(json!({})));
        let line = encode_notification(method::EVENT, &json!({"topic": "test:a"}));
        assert_eq!(parse_request(&line).unwrap().id, None);

        // Over-long lines.
        let long = format!(
            r#"{{"jsonrpc":"2.0","method":"m","params":"{}"}}"#,
            "a".repeat(MAX_LINE_BYTES)
        );
        let (_, err) = parse_request(&long).unwrap_err();
        assert_eq!(err.code, rpc_code::LINE_TOO_LONG);
    }

    #[test] // spec: ARCH-025, QA-010
    fn server_lines_round_trip() {
        let ok = Response::Result {
            id: Id::Num(1),
            result: json!({"status": "ok", "draining": true}),
        };
        let line = encode_response(&ok);
        assert!(!line.ends_with('\n'));
        assert_eq!(parse_server_line(&line).unwrap(), ServerLine::Response(ok));

        let err = Response::Error {
            id: None,
            error: RpcError {
                code: rpc_code::PARSE_ERROR,
                message: "parse error".into(),
                data: None,
            },
        };
        let line = encode_response(&err);
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["id"], serde_json::Value::Null);
        assert_eq!(v["jsonrpc"], "2.0");
        assert_eq!(parse_server_line(&line).unwrap(), ServerLine::Response(err));

        let note = encode_notification(method::EVENT, &json!({"topic": "test:a"}));
        match parse_server_line(&note).unwrap() {
            ServerLine::Notification(r) => assert_eq!(r.method, "lil/event"),
            other => panic!("{other:?}"),
        }

        for bad in [
            "nope",
            "[]",
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"id":1,"result":{}}"#,
        ] {
            assert!(parse_server_line(bad).is_err(), "{bad}");
        }
        assert_eq!(
            [
                method::HELLO,
                method::DRAIN,
                method::TOOLS_LIST,
                method::TOOLS_CALL
            ],
            ["core/hello", "core/drain", "tools/list", "tools/call"]
        );
        assert_eq!(
            [
                method::SUBSCRIBE,
                method::UNSUBSCRIBE,
                method::EVENT,
                method::APP_PREFIX
            ],
            ["lil/subscribe", "lil/unsubscribe", "lil/event", "app/"]
        );
        assert_eq!(MAX_LINE_BYTES, 4 * 1024 * 1024);
    }

    #[test] // spec: ARCH-031
    fn rpc_methods_are_lil_prefixed() {
        // The reserved prefixes: the event methods' and the app-only methods'.
        assert_eq!([method::PREFIX, method::APP_PREFIX], ["lil/", "app/"]);
        for m in [method::SUBSCRIBE, method::UNSUBSCRIBE, method::EVENT] {
            assert!(m.starts_with(method::PREFIX), "{m}");
        }
    }

    #[test] // spec: ARCH-025
    fn outbound_lines_respect_the_cap() {
        assert!(fits_line(&"a".repeat(MAX_LINE_BYTES)));
        assert!(!fits_line(&"a".repeat(MAX_LINE_BYTES + 1)));
        // A result leaves room for its envelope.
        assert!(fits_response_result(
            &"a".repeat(MAX_LINE_BYTES - RESPONSE_ENVELOPE_RESERVE)
        ));
        assert!(!fits_response_result(
            &"a".repeat(MAX_LINE_BYTES - RESPONSE_ENVELOPE_RESERVE + 1)
        ));

        // A response that fits is encoded as it is.
        let small = Response::Result {
            id: Id::Num(3),
            result: json!({"status": "ok"}),
        };
        assert_eq!(
            encode_response_capped(&small),
            (encode_response(&small), None)
        );

        // One that does not becomes -32603 for the same id, so the caller is not left waiting.
        let big = Response::Result {
            id: Id::Str("req-1".into()),
            result: json!({"status": "ok", "text": "a".repeat(MAX_LINE_BYTES)}),
        };
        let full = encode_response(&big).len();
        let (line, oversized) = encode_response_capped(&big);
        assert_eq!(oversized, Some(full));
        assert!(fits_line(&line));
        match parse_server_line(&line).unwrap() {
            ServerLine::Response(Response::Error { id, error }) => {
                assert_eq!(id, Some(Id::Str("req-1".into())));
                assert_eq!(error.code, rpc_code::INTERNAL_ERROR);
                assert!(error.message.contains("too large"), "{}", error.message);
            }
            other => panic!("{other:?}"),
        }

        // An id too long to echo in the error is replaced by null.
        let long_id = Response::Error {
            id: Some(Id::Str("i".repeat(MAX_LINE_BYTES))),
            error: RpcError::new(rpc_code::INVALID_PARAMS, "bad"),
        };
        let (line, oversized) = encode_response_capped(&long_id);
        assert!(oversized.is_some());
        assert!(fits_line(&line));
        match parse_server_line(&line).unwrap() {
            ServerLine::Response(Response::Error { id, error }) => {
                assert_eq!(id, None);
                assert_eq!(error.code, rpc_code::INTERNAL_ERROR);
            }
            other => panic!("{other:?}"),
        }
    }
}
