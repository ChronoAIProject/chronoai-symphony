//! Approval and tool-call policy for the Codex app-server protocol.
//!
//! Based on the OpenAI Symphony reference implementation's handling of
//! approval requests, tool calls, and user input signals.

use serde_json::Value;

/// Approval request methods that should be auto-approved.
const APPROVAL_METHODS: &[&str] = &[
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "execCommandApproval",
    "applyPatchApproval",
];

/// Methods that indicate user input is required.
const INPUT_REQUIRED_METHODS: &[&str] = &[
    "item/tool/requestUserInput",
    "turn/input_required",
    "turn/needs_input",
    "turn/need_input",
    "turn/request_input",
    "turn/request_response",
    "turn/provide_input",
    "turn/approval_required",
];

/// Check if the given method is an approval request that should be
/// auto-approved in a non-interactive session.
pub fn is_approval_request(method: &str) -> bool {
    APPROVAL_METHODS.iter().any(|m| *m == method)
}

/// Check if the given method is a tool call request (`item/tool/call`).
pub fn is_tool_call(method: &str) -> bool {
    method == "item/tool/call"
}

/// Check if the given method is a user-input-required signal.
pub fn is_user_input_request(method: &str, params: &Value) -> bool {
    if INPUT_REQUIRED_METHODS.iter().any(|m| *m == method) {
        return true;
    }

    // Check for turn/* methods with input-required flags in the payload.
    if method.starts_with("turn/") {
        if has_input_required_flag(params) {
            return true;
        }
        if let Some(inner) = params.get("params") {
            if has_input_required_flag(inner) {
                return true;
            }
        }
    }

    false
}

/// Return the approval decision string for the given method.
///
/// Uses `"acceptForSession"` for `item/*` methods and `"approved_for_session"`
/// for legacy methods, matching the OpenAI reference implementation.
pub fn approval_decision(method: &str) -> &'static str {
    match method {
        "item/commandExecution/requestApproval" | "item/fileChange/requestApproval" => {
            "acceptForSession"
        }
        _ => "approved_for_session",
    }
}

fn has_input_required_flag(value: &Value) -> bool {
    value.get("requiresInput") == Some(&Value::Bool(true))
        || value.get("needsInput") == Some(&Value::Bool(true))
        || value.get("input_required") == Some(&Value::Bool(true))
        || value.get("inputRequired") == Some(&Value::Bool(true))
        || value.get("type").and_then(|v| v.as_str()) == Some("input_required")
        || value.get("type").and_then(|v| v.as_str()) == Some("needs_input")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn approval_command_execution() {
        assert!(is_approval_request("item/commandExecution/requestApproval"));
    }

    #[test]
    fn approval_file_change() {
        assert!(is_approval_request("item/fileChange/requestApproval"));
    }

    #[test]
    fn approval_legacy_exec() {
        assert!(is_approval_request("execCommandApproval"));
    }

    #[test]
    fn approval_legacy_patch() {
        assert!(is_approval_request("applyPatchApproval"));
    }

    #[test]
    fn not_approval() {
        assert!(!is_approval_request("turn/completed"));
        assert!(!is_approval_request("notification"));
    }

    #[test]
    fn tool_call_method() {
        assert!(is_tool_call("item/tool/call"));
        assert!(!is_tool_call("item/tool/requestUserInput"));
    }

    #[test]
    fn input_required_direct_method() {
        assert!(is_user_input_request("item/tool/requestUserInput", &json!({})));
        assert!(is_user_input_request("turn/input_required", &json!({})));
        assert!(is_user_input_request("turn/needs_input", &json!({})));
    }

    #[test]
    fn input_required_via_flag() {
        assert!(is_user_input_request("turn/something", &json!({"requiresInput": true})));
        assert!(is_user_input_request("turn/something", &json!({"needsInput": true})));
        assert!(is_user_input_request("turn/something", &json!({"inputRequired": true})));
        assert!(is_user_input_request("turn/x", &json!({"type": "input_required"})));
    }

    #[test]
    fn input_not_required() {
        assert!(!is_user_input_request("turn/completed", &json!({})));
        assert!(!is_user_input_request("notification", &json!({"requiresInput": true})));
    }

    #[test]
    fn decision_for_item_methods() {
        assert_eq!(approval_decision("item/commandExecution/requestApproval"), "acceptForSession");
        assert_eq!(approval_decision("item/fileChange/requestApproval"), "acceptForSession");
    }

    #[test]
    fn decision_for_legacy_methods() {
        assert_eq!(approval_decision("execCommandApproval"), "approved_for_session");
        assert_eq!(approval_decision("applyPatchApproval"), "approved_for_session");
    }
}
