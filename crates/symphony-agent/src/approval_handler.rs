//! Approval handler trait and default implementation.
//!
//! Allows the orchestrator to control how approval requests from the agent
//! process are resolved. The default `AutoApproveHandler` approves all
//! requests immediately, matching the original behavior. The orchestrator
//! can provide a custom implementation that queues approvals for human
//! review via the HTTP API.

use async_trait::async_trait;
use serde_json::Value;

use crate::approval::approval_decision;

/// The decision returned by an approval handler.
#[derive(Debug, Clone)]
pub enum ApprovalDecision {
    /// Approve the request with the given decision string
    /// (e.g. "acceptForSession" or "approved_for_session").
    Approve(String),
    /// Deny the request.
    Deny,
}

/// Trait for handling approval requests from the agent process.
///
/// Implementors receive the raw approval request data and return a
/// decision. The handler may block (via `.await`) until a human
/// operator resolves the request through an external interface.
#[async_trait]
pub trait ApprovalHandler: Send + Sync {
    async fn handle_approval(
        &self,
        approval_id: String,
        method: String,
        params: Value,
    ) -> ApprovalDecision;
}

/// Default handler that auto-approves all requests immediately.
///
/// Uses `approval_decision()` to select the correct decision string
/// for the given method, matching the original hard-coded behavior.
pub struct AutoApproveHandler;

#[async_trait]
impl ApprovalHandler for AutoApproveHandler {
    async fn handle_approval(
        &self,
        _approval_id: String,
        method: String,
        _params: Value,
    ) -> ApprovalDecision {
        let decision = approval_decision(&method).to_string();
        ApprovalDecision::Approve(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn auto_approve_item_command_execution() {
        let handler = AutoApproveHandler;
        let result = handler
            .handle_approval(
                "req-1".to_string(),
                "item/commandExecution/requestApproval".to_string(),
                json!({}),
            )
            .await;

        match result {
            ApprovalDecision::Approve(decision) => {
                assert_eq!(decision, "acceptForSession");
            }
            ApprovalDecision::Deny => panic!("expected Approve"),
        }
    }

    #[tokio::test]
    async fn auto_approve_item_file_change() {
        let handler = AutoApproveHandler;
        let result = handler
            .handle_approval(
                "req-2".to_string(),
                "item/fileChange/requestApproval".to_string(),
                json!({"file": "main.rs"}),
            )
            .await;

        match result {
            ApprovalDecision::Approve(decision) => {
                assert_eq!(decision, "acceptForSession");
            }
            ApprovalDecision::Deny => panic!("expected Approve"),
        }
    }

    #[tokio::test]
    async fn auto_approve_legacy_method() {
        let handler = AutoApproveHandler;
        let result = handler
            .handle_approval(
                "req-3".to_string(),
                "execCommandApproval".to_string(),
                json!({}),
            )
            .await;

        match result {
            ApprovalDecision::Approve(decision) => {
                assert_eq!(decision, "approved_for_session");
            }
            ApprovalDecision::Deny => panic!("expected Approve"),
        }
    }

    #[tokio::test]
    async fn auto_approve_apply_patch() {
        let handler = AutoApproveHandler;
        let result = handler
            .handle_approval(
                "req-4".to_string(),
                "applyPatchApproval".to_string(),
                json!({}),
            )
            .await;

        match result {
            ApprovalDecision::Approve(decision) => {
                assert_eq!(decision, "approved_for_session");
            }
            ApprovalDecision::Deny => panic!("expected Approve"),
        }
    }
}
