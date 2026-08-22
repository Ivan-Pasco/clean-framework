//! JSON-RPC 2.0 over stdio, in the shape MCP requires.
//!
//! Hand-written rather than pulled from a crate. The wire format is small —
//! request, response, error, one message per line — and an SDK would pin us to
//! its own lifecycle and threading model for a server that has neither. What
//! we would gain is schema types we already have to write by hand anyway,
//! because the tool schemas come from Framework §10.2, not from a library.
//!
//! # Framing
//!
//! One JSON object per line, newline-delimited, until stdin closes. This is
//! the framing the compiler's own `--serve` adapter uses (Platform 14
//! §14.2.3), and matching it means a caller that can drive one can drive the
//! other.
//!
//! # Notifications
//!
//! A message with no `id` is a notification: the client is telling us
//! something and does not want an answer. MCP sends `notifications/initialized`
//! after the handshake. Replying to one is a protocol violation, so
//! [`Request::is_notification`] exists to make that check impossible to
//! forget at the call site.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The JSON-RPC version every message carries.
pub const JSONRPC_VERSION: &str = "2.0";

/// The MCP protocol revision this server implements.
///
/// Reported verbatim in the `initialize` result. A client asking for a
/// different revision still gets this one — the spec's negotiation rule is
/// that the server states what it speaks, and the client decides whether to
/// proceed.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Clone, Debug, Deserialize)]
pub struct Request {
    #[allow(dead_code)]
    pub jsonrpc: String,
    /// Absent for notifications. Present for calls that expect a response.
    #[serde(default)]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    /// A notification wants no response, and sending one is a protocol error.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

/// The JSON-RPC error codes this server can emit.
///
/// The first four are the standard set. `INVALID_PARAMS` covers a tool called
/// with arguments that do not match its schema; a tool that runs and *fails*
/// is not an error at this layer — it returns a result with `isError` set, so
/// the model can read the failure rather than the transport swallowing it.
pub mod error_code {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// One outgoing message.
#[derive(Clone, Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl Response {
    pub fn result(id: Value, result: Value) -> Self {
        Response { jsonrpc: JSONRPC_VERSION, id, result: Some(result), error: None }
    }

    pub fn error(id: Value, code: i64, message: impl Into<String>) -> Self {
        Response {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(ResponseError { code, message: message.into(), data: None }),
        }
    }

    /// An error carrying structured detail the model can act on.
    pub fn error_with_data(
        id: Value,
        code: i64,
        message: impl Into<String>,
        data: Value,
    ) -> Self {
        Response {
            jsonrpc: JSONRPC_VERSION,
            id,
            result: None,
            error: Some(ResponseError {
                code,
                message: message.into(),
                data: Some(data),
            }),
        }
    }
}

/// What a tool handler returns.
///
/// MCP wants tool output as a `content` array. We always emit exactly one
/// text block holding JSON, and mirror the same value in `structuredContent`
/// so a client that understands the newer field does not have to re-parse a
/// string. `isError` distinguishes "the tool ran and reports a failure" from
/// "the call itself was malformed", which is a JSON-RPC error instead.
#[derive(Clone, Debug)]
pub struct ToolOutcome {
    pub value: Value,
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(value: Value) -> Self {
        ToolOutcome { value, is_error: false }
    }

    /// A tool that ran and could not answer.
    ///
    /// This is deliberately *not* a JSON-RPC error: the model asked a
    /// reasonable question and deserves to read why it could not be answered,
    /// rather than seeing the transport reject the call.
    pub fn failed(value: Value) -> Self {
        ToolOutcome { value, is_error: true }
    }

    pub fn to_result(&self) -> Value {
        // `to_string` rather than a pretty print: this is read by a model, and
        // the structured mirror below is what a client parses.
        let text = serde_json::to_string(&self.value).unwrap_or_else(|_| "{}".into());
        json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": self.value,
            "isError": self.is_error,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_without_an_id_is_a_notification() {
        let raw = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        let request: Request = serde_json::from_str(raw).unwrap();
        assert!(request.is_notification());
    }

    #[test]
    fn a_message_with_an_id_expects_a_response() {
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let request: Request = serde_json::from_str(raw).unwrap();
        assert!(!request.is_notification());
    }

    #[test]
    fn absent_params_deserialize_rather_than_failing() {
        // `tools/list` is routinely sent with no params at all.
        let raw = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let request: Request = serde_json::from_str(raw).unwrap();
        assert!(request.params.is_null());
    }

    #[test]
    fn a_result_response_omits_the_error_field_entirely() {
        // A response carrying both `result` and `error: null` is malformed
        // JSON-RPC; strict clients reject it.
        let response = Response::result(json!(1), json!({"ok": true}));
        let encoded = serde_json::to_string(&response).unwrap();
        assert!(!encoded.contains("error"), "{encoded}");
    }

    #[test]
    fn an_error_response_omits_the_result_field_entirely() {
        let response = Response::error(json!(1), error_code::METHOD_NOT_FOUND, "nope");
        let encoded = serde_json::to_string(&response).unwrap();
        assert!(!encoded.contains("result"), "{encoded}");
    }

    #[test]
    fn a_tool_outcome_carries_the_same_value_in_both_shapes() {
        let outcome = ToolOutcome::ok(json!({"host": "clean-server"}));
        let result = outcome.to_result();

        assert_eq!(result["structuredContent"]["host"], "clean-server");
        assert_eq!(result["isError"], false);

        // The text block must be the same value, parseable back.
        let text = result["content"][0]["text"].as_str().unwrap();
        let reparsed: Value = serde_json::from_str(text).unwrap();
        assert_eq!(reparsed, outcome.value);
    }

    #[test]
    fn a_failed_tool_is_flagged_but_still_a_result() {
        let outcome = ToolOutcome::failed(json!({"error": "no project"}));
        assert_eq!(outcome.to_result()["isError"], true);
    }
}
