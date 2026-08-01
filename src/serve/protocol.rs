//! The JSON-RPC 2.0 wire contract of `h serve`.
//!
//! One message per line, UTF-8 JSON (`jsonl`), on stdout only. Logging goes to
//! the file appender, so stdout stays a clean protocol channel.

use h_core::{event::AgentViewEvent, interaction::AskQuestion};
use serde::Deserialize;
use serde_json::{Value, json};

/// Bumped when the protocol changes incompatibly; the client fails fast on
/// mismatch.
pub const PROTOCOL_VERSION: u32 = 1;

// Standard JSON-RPC error codes.
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
// Application error codes.
pub const SESSION_NOT_FOUND: i64 = -32000;
pub const RESUME_REFUSED: i64 = -32002;
pub const PROFILE_ERROR: i64 = -32003;
pub const INIT_ERROR: i64 = -32004;

/// A decoded incoming line, classified by which fields are present.
///
/// `method` present means a request (with `id`) or notification (without);
/// `method` absent with an `id` means a response to one of our requests.
#[derive(Debug, Deserialize)]
pub struct RpcIn {
    #[serde(default)]
    pub id: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
    #[serde(default)]
    pub result: Option<Value>,
}

impl RpcIn {
    pub fn is_request(&self) -> bool {
        self.method.is_some() && self.id.is_some()
    }

    pub fn is_response(&self) -> bool {
        self.method.is_none() && self.id.is_some()
    }
}

/// The application error a request failed with.
#[derive(Debug, Clone)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RpcError {}

pub fn hello(version: &str, pid: u32) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "server/hello",
        "params": { "protocol_version": PROTOCOL_VERSION, "version": version, "pid": pid },
    })
    .to_string()
}

pub fn response(id: &Value, result: Value) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

pub fn error_response(id: Option<&Value>, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

pub fn session_started(session_id: &str, model: &str, thinking_effort: Option<&str>) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "session/started",
        "params": {
            "session_id": session_id,
            "model": model,
            "thinking_effort": thinking_effort,
        },
    })
    .to_string()
}

pub fn session_event(session_id: &str, event: &AgentViewEvent) -> String {
    json!({
        "jsonrpc": "2.0",
        "method": "session/event",
        "params": { "session_id": session_id, "event": event },
    })
    .to_string()
}

pub fn ask_question(id: u64, session_id: &str, question: &AskQuestion) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "ask/question",
        "params": {
            "session_id": session_id,
            "question": question.question,
            "options": question.options,
        },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hello_carries_the_protocol_and_binary_versions() {
        let line = hello("0.3.0", 42);
        let value: Value = serde_json::from_str(&line).unwrap();

        assert_eq!(value["method"], "server/hello");
        assert_eq!(value["params"]["protocol_version"], PROTOCOL_VERSION);
        assert_eq!(value["params"]["version"], "0.3.0");
        assert_eq!(value["params"]["pid"], 42);
    }

    #[test]
    fn responses_and_errors_keep_the_request_id() {
        assert_eq!(
            serde_json::from_str::<Value>(&response(&json!(7), json!({"ok": true}))).unwrap(),
            json!({"jsonrpc": "2.0", "id": 7, "result": {"ok": true}})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&error_response(Some(&json!("a")), -32000, "gone"))
                .unwrap(),
            json!({"jsonrpc": "2.0", "id": "a", "error": {"code": -32000, "message": "gone"}})
        );
        assert_eq!(
            serde_json::from_str::<Value>(&error_response(None, -32700, "boom")).unwrap(),
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "boom"}})
        );
    }

    #[test]
    fn classification_distinguishes_requests_from_responses() {
        let request: RpcIn =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"session/list","params":{}}"#)
                .unwrap();
        assert!(request.is_request());
        assert!(!request.is_response());

        let response: RpcIn =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":17,"result":{"answer":{}}}"#).unwrap();
        assert!(response.is_response());
        assert!(!response.is_request());

        let notification: RpcIn =
            serde_json::from_str(r#"{"jsonrpc":"2.0","method":"session/event"}"#).unwrap();
        assert!(!notification.is_request());
        assert!(!notification.is_response());
    }
}
