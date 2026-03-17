//! JSON-RPC message types for the Codex app-server protocol.
//!
//! Provides structured request, response, and notification types along with
//! builder functions for constructing the specific messages required by the
//! handshake and turn lifecycle.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A JSON-RPC 2.0 request with a numeric ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub id: u64,
    pub method: String,
    pub params: Value,
}

/// A JSON-RPC 2.0 response, which may also carry notification-style fields.
///
/// The `method` and `params` fields support server-initiated notifications
/// that arrive on the same stream as responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub id: Option<u64>,
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub error: Option<Value>,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub params: Option<Value>,
}

/// A JSON-RPC 2.0 notification (no ID, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// Build an `initialize` request for session setup.
pub fn build_initialize(id: u64) -> JsonRpcRequest {
    JsonRpcRequest {
        id,
        method: "initialize".to_string(),
        params: json!({
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {
                "name": "symphony",
                "version": "0.1.0"
            }
        }),
    }
}

/// Build an `initialized` notification sent after the initialize response.
pub fn build_initialized() -> JsonRpcNotification {
    JsonRpcNotification {
        method: "notifications/initialized".to_string(),
        params: json!({}),
    }
}

/// Build a `thread/start` request to create a new conversation thread.
pub fn build_thread_start(
    id: u64,
    approval_policy: &str,
    sandbox: &str,
    cwd: &str,
) -> JsonRpcRequest {
    JsonRpcRequest {
        id,
        method: "thread/start".to_string(),
        params: json!({
            "approvalPolicy": approval_policy,
            "sandbox": sandbox,
            "cwd": cwd,
        }),
    }
}

/// Build a `turn/start` request to begin a new turn within a thread.
pub fn build_turn_start(
    id: u64,
    thread_id: &str,
    prompt: &str,
    cwd: &str,
    title: &str,
    approval_policy: &str,
    sandbox_policy: &str,
) -> JsonRpcRequest {
    JsonRpcRequest {
        id,
        method: "turn/start".to_string(),
        params: json!({
            "threadId": thread_id,
            "prompt": prompt,
            "cwd": cwd,
            "title": title,
            "approvalPolicy": approval_policy,
            "sandboxPolicy": sandbox_policy,
        }),
    }
}

/// Build an approval response for a pending tool-call approval request.
pub fn build_approval_response(id: &str, approved: bool) -> Value {
    json!({
        "id": id,
        "result": {
            "approved": approved,
        }
    })
}

/// Build a tool-call failure response indicating an unsupported operation.
pub fn build_tool_call_failure(id: &str, error: &str) -> Value {
    json!({
        "id": id,
        "error": {
            "code": -32601,
            "message": error,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_request_has_correct_method() {
        let req = build_initialize(1);
        assert_eq!(req.method, "initialize");
        assert_eq!(req.id, 1);
        assert!(req.params["protocolVersion"].is_string());
    }

    #[test]
    fn initialized_notification_has_correct_method() {
        let notif = build_initialized();
        assert_eq!(notif.method, "notifications/initialized");
    }

    #[test]
    fn thread_start_includes_all_params() {
        let req = build_thread_start(2, "full-auto", "none", "/tmp/ws");
        assert_eq!(req.method, "thread/start");
        assert_eq!(req.params["approvalPolicy"], "full-auto");
        assert_eq!(req.params["sandbox"], "none");
        assert_eq!(req.params["cwd"], "/tmp/ws");
    }

    #[test]
    fn turn_start_includes_all_params() {
        let req = build_turn_start(
            3,
            "thread-1",
            "Fix the bug",
            "/tmp/ws",
            "Bug fix",
            "full-auto",
            "none",
        );
        assert_eq!(req.method, "turn/start");
        assert_eq!(req.params["threadId"], "thread-1");
        assert_eq!(req.params["prompt"], "Fix the bug");
    }

    #[test]
    fn approval_response_approved() {
        let resp = build_approval_response("req-1", true);
        assert_eq!(resp["id"], "req-1");
        assert_eq!(resp["result"]["approved"], true);
    }

    #[test]
    fn approval_response_denied() {
        let resp = build_approval_response("req-2", false);
        assert_eq!(resp["result"]["approved"], false);
    }

    #[test]
    fn tool_call_failure_includes_error() {
        let resp = build_tool_call_failure("req-3", "unsupported tool");
        assert_eq!(resp["id"], "req-3");
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["error"]["message"], "unsupported tool");
    }

    #[test]
    fn request_serialization_roundtrip() {
        let req = build_initialize(42);
        let json_str = serde_json::to_string(&req).unwrap();
        let parsed: JsonRpcRequest = serde_json::from_str(&json_str).unwrap();
        assert_eq!(parsed.id, 42);
        assert_eq!(parsed.method, "initialize");
    }

    #[test]
    fn response_deserializes_with_defaults() {
        let json_str = r#"{"id": 1, "result": {"ok": true}}"#;
        let resp: JsonRpcResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.id, Some(1));
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
        assert!(resp.method.is_none());
    }
}
