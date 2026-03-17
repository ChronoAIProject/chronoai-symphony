//! JSON-RPC message types for the Codex app-server protocol.
//!
//! Based on the OpenAI Symphony reference implementation's exact message
//! formats for initialize, thread/start, and turn/start.

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

/// Build an `initialize` request matching OpenAI's format.
pub fn build_initialize(id: u64) -> JsonRpcRequest {
    JsonRpcRequest {
        id,
        method: "initialize".to_string(),
        params: json!({
            "capabilities": {
                "experimentalApi": true
            },
            "clientInfo": {
                "name": "symphony-orchestrator",
                "title": "Symphony Orchestrator",
                "version": "0.1.0"
            }
        }),
    }
}

/// Build an `initialized` notification sent after the initialize response.
pub fn build_initialized() -> JsonRpcNotification {
    JsonRpcNotification {
        method: "initialized".to_string(),
        params: json!({}),
    }
}

/// Build a `thread/start` request.
///
/// `approval_policy` and `sandbox` can be either JSON strings or objects.
/// OpenAI defaults:
/// - approval_policy: `{"reject": {"sandbox_approval": true, "rules": true, "mcp_elicitations": true}}`
/// - sandbox: `"workspace-write"`
pub fn build_thread_start(
    id: u64,
    approval_policy: &Value,
    sandbox: &Value,
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

/// Build a `turn/start` request matching OpenAI's format.
///
/// The prompt is wrapped in `input: [{type: "text", text: prompt}]`.
/// `approval_policy` and `sandbox_policy` can be strings or objects.
pub fn build_turn_start(
    id: u64,
    thread_id: &str,
    prompt: &str,
    cwd: &str,
    title: &str,
    approval_policy: &Value,
    sandbox_policy: &Value,
) -> JsonRpcRequest {
    JsonRpcRequest {
        id,
        method: "turn/start".to_string(),
        params: json!({
            "threadId": thread_id,
            "input": [
                {
                    "type": "text",
                    "text": prompt
                }
            ],
            "cwd": cwd,
            "title": title,
            "approvalPolicy": approval_policy,
            "sandboxPolicy": sandbox_policy,
        }),
    }
}

/// Default approval policy for automated agent sessions.
///
/// Uses `"never"` which means the agent never asks for approval and
/// auto-approves all actions. This is the standard choice for
/// non-interactive orchestrators. Valid values accepted by codex:
/// `untrusted`, `on-failure`, `on-request`, `granular`, `never`.
pub fn default_approval_policy() -> Value {
    json!("never")
}

/// Default thread sandbox value.
pub fn default_thread_sandbox() -> Value {
    json!("workspace-write")
}

/// Build a default turn sandbox policy for a given workspace path.
pub fn default_turn_sandbox_policy(workspace_path: &str) -> Value {
    json!({
        "type": "workspaceWrite",
        "writableRoots": [workspace_path],
        "readOnlyAccess": {"type": "fullAccess"},
        "networkAccess": false,
        "excludeTmpdirEnvVar": false,
        "excludeSlashTmp": false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_has_experimental_api() {
        let req = build_initialize(1);
        assert_eq!(req.method, "initialize");
        assert_eq!(req.params["capabilities"]["experimentalApi"], true);
        assert_eq!(req.params["clientInfo"]["name"], "symphony-orchestrator");
    }

    #[test]
    fn initialized_method_name() {
        let notif = build_initialized();
        assert_eq!(notif.method, "initialized");
    }

    #[test]
    fn thread_start_with_default_policy() {
        let policy = default_approval_policy();
        let sandbox = default_thread_sandbox();
        let req = build_thread_start(2, &policy, &sandbox, "/tmp/ws");
        assert_eq!(req.method, "thread/start");
        assert_eq!(req.params["approvalPolicy"], "never");
        assert_eq!(req.params["sandbox"], "workspace-write");
    }

    #[test]
    fn turn_start_wraps_prompt_in_input() {
        let policy = default_approval_policy();
        let sandbox = default_turn_sandbox_policy("/tmp/ws");
        let req = build_turn_start(3, "thread-1", "Fix the bug", "/tmp/ws", "#42: Bug", &policy, &sandbox);
        assert_eq!(req.method, "turn/start");
        let input = req.params["input"].as_array().unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "text");
        assert_eq!(input[0]["text"], "Fix the bug");
    }

    #[test]
    fn default_turn_sandbox_has_workspace_write() {
        let policy = default_turn_sandbox_policy("/tmp/ws/_42");
        assert_eq!(policy["type"], "workspaceWrite");
        assert_eq!(policy["writableRoots"][0], "/tmp/ws/_42");
    }
}
