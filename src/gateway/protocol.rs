//! JSON-RPC 2.0 as the gateway speaks it: one JSON message per line,
//! requests (with an `id`) get a response, and the gateway pushes
//! notifications (no `id`), e.g. `stopping` before it exits.

use serde::Deserialize;
use serde_json::{Value, json};

/// The longest line either side accepts (a message is one line).
pub const MAX_LINE: usize = 1 << 20;

/// JSON-RPC's own error codes, and the gateway's (from -32000).
pub mod code {
    pub const PARSE: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    /// Something other than `hello` came first.
    pub const HELLO_FIRST: i64 = -32001;
    /// The client speaks another protocol version.
    pub const VERSION: i64 = -32002;
    /// A model couldn't answer: none set up, no key, the provider failed.
    pub const MODEL: i64 = -32010;
    /// A database couldn't be read: no such connection, couldn't connect.
    pub const DATABASE: i64 = -32011;
    /// The project doesn't share its schemas with that model (one that
    /// isn't on this machine): the user may allow it.
    pub const NOT_SHARED: i64 = -32012;
    /// The project's memory couldn't be read or changed.
    pub const MEMORY: i64 = -32013;
}

/// A request or notification from a client.
#[derive(Debug, Deserialize)]
pub struct Incoming {
    pub jsonrpc: String,
    /// Absent (or null) for a notification, which gets no response.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// An error as a response carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    /// What a client may act on (JSON-RPC's `data`).
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError { code, message: message.into(), data: None }
    }

    pub fn with_data(code: i64, message: impl Into<String>, data: Value) -> Self {
        RpcError { code, message: message.into(), data: Some(data) }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// A line: the message, then `\n`.
fn line(message: &Value) -> String {
    let mut text = message.to_string();
    text.push('\n');
    text
}

pub fn request(id: u64, method: &str, params: &Value) -> String {
    line(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
}

pub fn result(id: &Value, result: &Value) -> String {
    line(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

pub fn error(id: &Value, error: &RpcError) -> String {
    let mut body = json!({ "code": error.code, "message": error.message });
    if let Some(data) = &error.data {
        body["data"] = data.clone();
    }
    line(&json!({ "jsonrpc": "2.0", "id": id, "error": body }))
}

pub fn notification(method: &str, params: &Value) -> String {
    line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

/// Parses a client's line: the message, or the error to answer with.
pub fn parse_incoming(text: &[u8]) -> Result<Incoming, RpcError> {
    let value: Value =
        serde_json::from_slice(text).map_err(|e| RpcError::new(code::PARSE, format!("not JSON: {e}")))?;
    let incoming: Incoming = serde_json::from_value(value)
        .map_err(|e| RpcError::new(code::INVALID_REQUEST, format!("not a JSON-RPC request: {e}")))?;
    if incoming.jsonrpc != "2.0" {
        return Err(RpcError::new(code::INVALID_REQUEST, "only JSON-RPC 2.0 is spoken here"));
    }
    Ok(incoming)
}

/// What the gateway sent a client.
#[derive(Debug, PartialEq)]
pub enum Outgoing {
    Response { id: Value, outcome: Result<Value, RpcError> },
    Notification { method: String, params: Value },
}

/// Parses a line from the gateway.
pub fn parse_outgoing(text: &[u8]) -> Result<Outgoing, String> {
    let value: Value = serde_json::from_slice(text).map_err(|e| format!("the gateway sent something not JSON: {e}"))?;
    if let Some(method) = value.get("method").and_then(Value::as_str) {
        let params = value.get("params").cloned().unwrap_or(Value::Null);
        return Ok(Outgoing::Notification { method: method.to_string(), params });
    }
    let id = value.get("id").cloned().unwrap_or(Value::Null);
    let outcome = match (value.get("result"), value.get("error")) {
        (Some(result), None) => Ok(result.clone()),
        (None, Some(error)) => Err(RpcError {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error.get("message").and_then(Value::as_str).unwrap_or("unknown error").to_string(),
            data: error.get("data").cloned(),
        }),
        _ => return Err("the gateway sent a response with neither a result nor an error".into()),
    };
    Ok(Outgoing::Response { id, outcome })
}
