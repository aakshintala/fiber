//! Newline-delimited JSON-RPC 2.0 (`docs/dependencies.md`, "Written
//! ourselves"): one JSON object per line, a `u64` id per request, never
//! reused.

use serde_json::Value;

/// A response to one of our requests: its id and its outcome.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Response {
    /// The request's id.
    pub id: u64,
    /// What the server answered.
    pub outcome: Outcome,
}

/// What a response carries.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    /// The `result`.
    Result(Value),
    /// The `error`'s code and message.
    Error {
        /// The JSON-RPC error code.
        code: i64,
        /// The JSON-RPC error message.
        message: String,
    },
}

/// A request from the server: `ping`, answered with `{}`, or any other
/// method, answered with JSON-RPC error `-32601`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ServerRequest {
    /// The request's id.
    pub id: RequestId,
    /// The method.
    pub method: String,
    /// Its params.
    pub params: Value,
}

/// A request id, as the wire carries it: a number or a string. Answers echo
/// it unchanged.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RequestId {
    /// A numeric id.
    Number(u64),
    /// A string id.
    Text(String),
}

/// What one stdout line carried.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Incoming {
    /// A response to one of our requests.
    Response(Response),
    /// A request from the server.
    ServerRequest(ServerRequest),
    /// A server notification, or anything else shaped like neither: ignored.
    Ignored,
}

/// Encodes a request: `{"jsonrpc":"2.0","id":<id>,"method":<method>,
/// "params":<params>}`, defaulting absent params to `{}`.
pub(crate) fn encode_request(id: u64, method: &str, params: Option<&Value>) -> String {
    let params = params.cloned().unwrap_or(Value::Object(Default::default()));
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// Encodes a notification: `{"jsonrpc":"2.0","method":<method>}`, with
/// `params` when present.
pub(crate) fn encode_notification(method: &str, params: Option<&Value>) -> String {
    match params {
        Some(params) => {
            serde_json::json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
        }
        None => serde_json::json!({"jsonrpc": "2.0", "method": method}).to_string(),
    }
}

/// Encodes a successful answer to a server request, echoing its id.
pub(crate) fn encode_result(id: &RequestId, result: &Value) -> String {
    match id {
        RequestId::Number(number) => {
            serde_json::json!({"jsonrpc": "2.0", "id": number, "result": result}).to_string()
        }
        RequestId::Text(text) => {
            serde_json::json!({"jsonrpc": "2.0", "id": text, "result": result}).to_string()
        }
    }
}

/// Encodes an error answer to a server request, echoing its id.
pub(crate) fn encode_error(id: &RequestId, code: i64, message: &str) -> String {
    match id {
        RequestId::Number(number) => {
            serde_json::json!({"jsonrpc": "2.0", "id": number, "error": {"code": code, "message": message}})
                .to_string()
        }
        RequestId::Text(text) => {
            serde_json::json!({"jsonrpc": "2.0", "id": text, "error": {"code": code, "message": message}})
                .to_string()
        }
    }
}

/// Reads one stdout line. A line that is not a JSON object is [`Incoming::Ignored`]:
/// garbage on stdout never fails a call.
pub(crate) fn decode_line(line: &str) -> Incoming {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(_) => return Incoming::Ignored,
    };
    let object = match value.as_object() {
        Some(object) => object,
        None => return Incoming::Ignored,
    };
    if let Some(method) = object.get("method").and_then(Value::as_str) {
        match read_id(object.get("id")) {
            Some(id) => Incoming::ServerRequest(ServerRequest {
                id,
                method: method.to_owned(),
                params: object.get("params").cloned().unwrap_or(Value::Null),
            }),
            None => Incoming::Ignored,
        }
    } else if let Some(id) = object.get("id").and_then(Value::as_u64) {
        if let Some(result) = object.get("result") {
            Incoming::Response(Response {
                id,
                outcome: Outcome::Result(result.clone()),
            })
        } else if let Some(error) = object.get("error").and_then(Value::as_object) {
            let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            Incoming::Response(Response {
                id,
                outcome: Outcome::Error { code, message },
            })
        } else {
            Incoming::Ignored
        }
    } else {
        Incoming::Ignored
    }
}

fn read_id(value: Option<&Value>) -> Option<RequestId> {
    match value {
        Some(Value::Number(number)) => number.as_u64().map(RequestId::Number),
        Some(Value::String(text)) => Some(RequestId::Text(text.clone())),
        Some(_) | None => None,
    }
}

#[cfg(test)]
#[path = "rpc_tests.rs"]
mod tests;
