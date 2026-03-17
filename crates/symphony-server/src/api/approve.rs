//! POST /api/v1/approve/{approval_id} endpoint.
//!
//! Resolves a pending approval request by accepting or denying it.
//! The approval queue is managed by the orchestrator and exposed
//! through the shared `AppState`.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use symphony_agent::approval;
use symphony_agent::approval_handler::ApprovalDecision;

use crate::router::AppState;

/// Request body for the approve/deny action.
#[derive(Debug, Deserialize)]
pub struct ApproveRequest {
    /// Must be `"approve"` or `"deny"`.
    pub decision: String,
}

/// Resolve a pending approval request.
///
/// # Path parameters
///
/// - `approval_id` - The unique identifier of the pending approval.
///
/// # Request body
///
/// ```json
/// { "decision": "approve" }
/// ```
///
/// The `decision` field must be either `"approve"` or `"deny"`.
///
/// # Responses
///
/// - `200 OK` with resolution details on success.
/// - `400 Bad Request` if the decision string is invalid.
/// - `404 Not Found` if the approval ID does not exist in the queue.
pub async fn post_approve(
    State(state): State<Arc<AppState>>,
    Path(approval_id): Path<String>,
    Json(body): Json<ApproveRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    match body.decision.as_str() {
        "approve" | "deny" => {}
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": {
                        "code": "invalid_decision",
                        "message": format!(
                            "Unknown decision: {other}. Use 'approve' or 'deny'."
                        )
                    }
                })),
            ));
        }
    }

    // For approvals, look up the pending entry to determine the correct
    // protocol decision string based on the original method. For denials
    // the method is irrelevant.
    let decision = if body.decision == "approve" {
        let method = state
            .approval_queue
            .list_pending()
            .iter()
            .find(|s| s.id == approval_id)
            .map(|s| s.method.clone());
        let decision_str = match method.as_deref() {
            Some(m) => approval::approval_decision(m).to_string(),
            None => "approved".to_string(),
        };
        ApprovalDecision::Approve(decision_str)
    } else {
        ApprovalDecision::Deny
    };

    match state.approval_queue.resolve(&approval_id, decision) {
        Ok(()) => Ok(Json(json!({
            "resolved": true,
            "approval_id": approval_id,
            "decision": body.decision,
        }))),
        Err(msg) => Err((
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": {
                    "code": "approval_not_found",
                    "message": msg,
                }
            })),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserialize_approve_request() {
        let json_str = r#"{"decision": "approve"}"#;
        let req: ApproveRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(req.decision, "approve");
    }

    #[test]
    fn deserialize_deny_request() {
        let json_str = r#"{"decision": "deny"}"#;
        let req: ApproveRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(req.decision, "deny");
    }
}
