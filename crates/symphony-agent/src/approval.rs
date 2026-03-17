//! Approval and tool-call policy for agent sessions.
//!
//! Determines whether incoming approval requests should be auto-approved
//! and detects when the agent requires user input to continue.

use serde_json::Value;

/// Methods that should be auto-approved without user interaction.
const AUTO_APPROVE_METHODS: &[&str] = &[
    "codex/approveExecution",
    "codex/approveFileChange",
    "codex/approveCommand",
    "codex/approveTool",
];

/// Methods that indicate user input is required.
const USER_INPUT_METHODS: &[&str] = &[
    "codex/requestInput",
    "codex/requestUserInput",
    "codex/userInputRequired",
];

/// Determine whether an approval request with the given method should
/// be auto-approved.
///
/// Returns `true` for command execution and file-change approval requests
/// that are safe to approve in fully automated mode.
pub fn should_auto_approve(method: &str) -> bool {
    AUTO_APPROVE_METHODS
        .iter()
        .any(|m| method.eq_ignore_ascii_case(m))
}

/// Detect whether a message indicates that user input is required.
///
/// Checks both the method name and common patterns in the params payload
/// that signal the agent cannot continue without human interaction.
pub fn is_user_input_request(method: &str, params: &Value) -> bool {
    // Direct method match.
    if USER_INPUT_METHODS
        .iter()
        .any(|m| method.eq_ignore_ascii_case(m))
    {
        return true;
    }

    // Check for input_required flag in params.
    if let Some(true) = params.get("inputRequired").and_then(|v| v.as_bool()) {
        return true;
    }

    if let Some(true) = params.get("input_required").and_then(|v| v.as_bool()) {
        return true;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn auto_approves_execution() {
        assert!(should_auto_approve("codex/approveExecution"));
    }

    #[test]
    fn auto_approves_file_change() {
        assert!(should_auto_approve("codex/approveFileChange"));
    }

    #[test]
    fn auto_approves_command() {
        assert!(should_auto_approve("codex/approveCommand"));
    }

    #[test]
    fn auto_approves_tool() {
        assert!(should_auto_approve("codex/approveTool"));
    }

    #[test]
    fn does_not_auto_approve_unknown() {
        assert!(!should_auto_approve("codex/unknownMethod"));
    }

    #[test]
    fn case_insensitive_auto_approve() {
        assert!(should_auto_approve("CODEX/APPROVEEXECUTION"));
    }

    #[test]
    fn detects_user_input_by_method() {
        assert!(is_user_input_request("codex/requestInput", &json!({})));
    }

    #[test]
    fn detects_user_input_by_params_flag() {
        assert!(is_user_input_request(
            "someMethod",
            &json!({"inputRequired": true})
        ));
    }

    #[test]
    fn detects_user_input_by_snake_case_flag() {
        assert!(is_user_input_request(
            "someMethod",
            &json!({"input_required": true})
        ));
    }

    #[test]
    fn not_user_input_for_normal_methods() {
        assert!(!is_user_input_request("codex/turnCompleted", &json!({})));
    }

    #[test]
    fn not_user_input_when_flag_is_false() {
        assert!(!is_user_input_request(
            "someMethod",
            &json!({"inputRequired": false})
        ));
    }
}
