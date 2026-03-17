//! POST /api/v1/refresh endpoint.
//!
//! Triggers an immediate poll-and-reconcile cycle in the orchestrator.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::router::AppState;

/// Request an immediate orchestrator refresh.
///
/// Sends a `RefreshRequested` event to the orchestrator's event loop,
/// which triggers an out-of-cycle poll and reconciliation pass.
///
/// Returns `202 Accepted` immediately -- the actual refresh happens
/// asynchronously.
///
/// # Response format
///
/// ```json
/// {
///     "queued": true,
///     "coalesced": false,
///     "requested_at": "2025-01-15T10:00:00Z",
///     "operations": ["poll", "reconcile"]
/// }
/// ```
pub async fn post_refresh(
    State(state): State<Arc<AppState>>,
) -> (StatusCode, Json<Value>) {
    let send_result = state
        .orchestrator_tx
        .send(symphony_orchestrator::events::OrchestratorEvent::RefreshRequested)
        .await;

    let queued = send_result.is_ok();

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "queued": queued,
            "coalesced": false,
            "requested_at": chrono::Utc::now().to_rfc3339(),
            "operations": ["poll", "reconcile"]
        })),
    )
}
