//! GET /api/v1/state endpoint.
//!
//! Returns the current orchestrator state as a JSON snapshot.

use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde_json::Value;

use crate::router::AppState;

/// Return the full orchestrator state snapshot.
///
/// The response includes running sessions, retry queue, aggregate token
/// totals, and rate limit information.
///
/// # Response format
///
/// ```json
/// {
///     "generated_at": "2025-01-15T10:00:00Z",
///     "counts": { "running": 2, "retrying": 1 },
///     "running": [ ... ],
///     "retrying": [ ... ],
///     "codex_totals": {
///         "input_tokens": 1000,
///         "output_tokens": 2000,
///         "total_tokens": 3000,
///         "seconds_running": 45.5
///     },
///     "rate_limits": null
/// }
/// ```
pub async fn get_state(
    State(state): State<Arc<AppState>>,
) -> Json<Value> {
    let snapshot = (state.snapshot_fn)();
    Json(snapshot)
}
