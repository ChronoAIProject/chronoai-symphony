//! Pending approval queue and queued approval handler.
//!
//! Provides a thread-safe queue of approval requests that are waiting for
//! human resolution via the HTTP API. The `QueuedApprovalHandler` blocks
//! the agent turn until the approval is resolved externally.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

use symphony_agent::approval_handler::{ApprovalDecision, ApprovalHandler};

/// Serializable summary of a pending approval for the HTTP API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingApprovalSummary {
    pub id: String,
    pub issue_id: String,
    pub issue_identifier: String,
    pub method: String,
    pub created_at: String,
}

/// A pending approval request with a one-shot channel for the response.
pub struct PendingApproval {
    pub id: String,
    pub issue_id: String,
    pub issue_identifier: String,
    pub method: String,
    pub params: Value,
    pub created_at: DateTime<Utc>,
    pub response_tx: oneshot::Sender<ApprovalDecision>,
}

/// Thread-safe queue of pending approval requests.
///
/// Approvals are inserted by `QueuedApprovalHandler` instances and
/// resolved by the HTTP API calling `resolve()`. The queue also supports
/// bulk removal when a worker exits.
pub struct PendingApprovalQueue {
    pending: Mutex<HashMap<String, PendingApproval>>,
}

impl PendingApprovalQueue {
    pub fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// Insert a new pending approval into the queue.
    pub fn insert(&self, approval: PendingApproval) {
        let mut guard = self.pending.lock().expect("approval queue lock poisoned");
        guard.insert(approval.id.clone(), approval);
    }

    /// Resolve a pending approval by ID, sending the decision to the
    /// waiting handler. Returns an error string if the ID is not found.
    pub fn resolve(&self, id: &str, decision: ApprovalDecision) -> Result<(), String> {
        let mut guard = self.pending.lock().expect("approval queue lock poisoned");
        let approval = guard
            .remove(id)
            .ok_or_else(|| format!("approval '{}' not found", id))?;
        // If the receiver has been dropped the send will fail, but we
        // treat that as success (the handler already moved on).
        let _ = approval.response_tx.send(decision);
        Ok(())
    }

    /// List all pending approvals as serializable summaries.
    pub fn list_pending(&self) -> Vec<PendingApprovalSummary> {
        let guard = self.pending.lock().expect("approval queue lock poisoned");
        guard
            .values()
            .map(|a| PendingApprovalSummary {
                id: a.id.clone(),
                issue_id: a.issue_id.clone(),
                issue_identifier: a.issue_identifier.clone(),
                method: a.method.clone(),
                created_at: a.created_at.to_rfc3339(),
            })
            .collect()
    }

    /// Remove all pending approvals for a given issue, dropping the
    /// response channels (which causes the handlers to receive `Deny`).
    pub fn remove_by_issue(&self, issue_id: &str) {
        let mut guard = self.pending.lock().expect("approval queue lock poisoned");
        guard.retain(|_, a| a.issue_id != issue_id);
    }
}

impl Default for PendingApprovalQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// An `ApprovalHandler` that queues approvals and waits for external
/// resolution through the `PendingApprovalQueue`.
pub struct QueuedApprovalHandler {
    queue: Arc<PendingApprovalQueue>,
    issue_id: String,
    issue_identifier: String,
}

impl QueuedApprovalHandler {
    pub fn new(
        queue: Arc<PendingApprovalQueue>,
        issue_id: String,
        issue_identifier: String,
    ) -> Self {
        Self {
            queue,
            issue_id,
            issue_identifier,
        }
    }
}

#[async_trait]
impl ApprovalHandler for QueuedApprovalHandler {
    async fn handle_approval(
        &self,
        approval_id: String,
        method: String,
        params: Value,
    ) -> ApprovalDecision {
        let (tx, rx) = oneshot::channel();
        self.queue.insert(PendingApproval {
            id: approval_id,
            issue_id: self.issue_id.clone(),
            issue_identifier: self.issue_identifier.clone(),
            method,
            params,
            created_at: Utc::now(),
            response_tx: tx,
        });
        // Wait for the decision from the HTTP endpoint.
        // If the sender is dropped (e.g. queue cleanup), default to Deny.
        rx.await.unwrap_or(ApprovalDecision::Deny)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_insert_and_list() {
        let queue = PendingApprovalQueue::new();
        let (tx, _rx) = oneshot::channel();
        queue.insert(PendingApproval {
            id: "a1".to_string(),
            issue_id: "issue-1".to_string(),
            issue_identifier: "PROJ-1".to_string(),
            method: "item/commandExecution/requestApproval".to_string(),
            params: Value::Null,
            created_at: Utc::now(),
            response_tx: tx,
        });

        let list = queue.list_pending();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, "a1");
        assert_eq!(list[0].issue_id, "issue-1");
    }

    #[test]
    fn queue_resolve_success() {
        let queue = PendingApprovalQueue::new();
        let (tx, mut rx) = oneshot::channel();
        queue.insert(PendingApproval {
            id: "a2".to_string(),
            issue_id: "issue-2".to_string(),
            issue_identifier: "PROJ-2".to_string(),
            method: "execCommandApproval".to_string(),
            params: Value::Null,
            created_at: Utc::now(),
            response_tx: tx,
        });

        let result = queue.resolve(
            "a2",
            ApprovalDecision::Approve("acceptForSession".to_string()),
        );
        assert!(result.is_ok());
        assert!(queue.list_pending().is_empty());

        // The receiver should have received the decision.
        let decision = rx.try_recv().unwrap();
        match decision {
            ApprovalDecision::Approve(d) => assert_eq!(d, "acceptForSession"),
            ApprovalDecision::Deny => panic!("expected Approve"),
        }
    }

    #[test]
    fn queue_resolve_not_found() {
        let queue = PendingApprovalQueue::new();
        let result = queue.resolve("nonexistent", ApprovalDecision::Deny);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not found"));
    }

    #[test]
    fn queue_remove_by_issue() {
        let queue = PendingApprovalQueue::new();

        for i in 0..3 {
            let (tx, _rx) = oneshot::channel();
            queue.insert(PendingApproval {
                id: format!("a{}", i),
                issue_id: if i < 2 {
                    "issue-1".to_string()
                } else {
                    "issue-2".to_string()
                },
                issue_identifier: format!("PROJ-{}", i),
                method: "test".to_string(),
                params: Value::Null,
                created_at: Utc::now(),
                response_tx: tx,
            });
        }

        assert_eq!(queue.list_pending().len(), 3);
        queue.remove_by_issue("issue-1");

        let remaining = queue.list_pending();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].issue_id, "issue-2");
    }

    #[tokio::test]
    async fn queued_handler_resolves_via_queue() {
        let queue = Arc::new(PendingApprovalQueue::new());
        let handler = QueuedApprovalHandler::new(
            Arc::clone(&queue),
            "issue-10".to_string(),
            "PROJ-10".to_string(),
        );

        let queue_clone = Arc::clone(&queue);
        // Spawn a task that resolves the approval after it appears.
        let resolver = tokio::spawn(async move {
            // Wait until the approval appears in the queue.
            loop {
                let pending = queue_clone.list_pending();
                if !pending.is_empty() {
                    let id = pending[0].id.clone();
                    queue_clone
                        .resolve(&id, ApprovalDecision::Approve("yes".to_string()))
                        .unwrap();
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        let decision = handler
            .handle_approval(
                "approval-99".to_string(),
                "item/commandExecution/requestApproval".to_string(),
                Value::Null,
            )
            .await;

        resolver.await.unwrap();

        match decision {
            ApprovalDecision::Approve(d) => assert_eq!(d, "yes"),
            ApprovalDecision::Deny => panic!("expected Approve"),
        }
    }

    #[tokio::test]
    async fn queued_handler_defaults_to_deny_on_drop() {
        let queue = Arc::new(PendingApprovalQueue::new());
        let handler = QueuedApprovalHandler::new(
            Arc::clone(&queue),
            "issue-11".to_string(),
            "PROJ-11".to_string(),
        );

        let queue_clone = Arc::clone(&queue);
        // Spawn a task that removes by issue (drops the sender).
        let dropper = tokio::spawn(async move {
            loop {
                let pending = queue_clone.list_pending();
                if !pending.is_empty() {
                    queue_clone.remove_by_issue("issue-11");
                    break;
                }
                tokio::task::yield_now().await;
            }
        });

        let decision = handler
            .handle_approval(
                "approval-100".to_string(),
                "execCommandApproval".to_string(),
                Value::Null,
            )
            .await;

        dropper.await.unwrap();

        match decision {
            ApprovalDecision::Deny => {} // expected
            ApprovalDecision::Approve(_) => panic!("expected Deny"),
        }
    }
}
