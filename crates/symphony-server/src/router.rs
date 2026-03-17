//! Axum router and HTTP server setup for the Symphony dashboard and API.

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use symphony_orchestrator::approval_queue::PendingApprovalQueue;

use crate::api;
use crate::dashboard;

/// Shared application state accessible from all request handlers.
///
/// Provides two capabilities to handlers:
/// - Sending events to the orchestrator via `orchestrator_tx`.
/// - Obtaining a point-in-time JSON snapshot of orchestrator state via `snapshot_fn`.
pub struct AppState {
    /// Channel for sending events to the orchestrator's main loop.
    pub orchestrator_tx:
        tokio::sync::mpsc::Sender<symphony_orchestrator::events::OrchestratorEvent>,

    /// Closure that returns the current orchestrator state as a JSON value.
    ///
    /// The snapshot conforms to the structure defined in the Symphony spec:
    /// ```json
    /// {
    ///     "generated_at": "...",
    ///     "counts": { "running": N, "retrying": N },
    ///     "running": [...],
    ///     "retrying": [...],
    ///     "codex_totals": { ... },
    ///     "rate_limits": null
    /// }
    /// ```
    pub snapshot_fn: Arc<dyn Fn() -> serde_json::Value + Send + Sync>,

    /// Shared pending approval queue for human-in-the-loop decisions.
    pub approval_queue: Arc<PendingApprovalQueue>,
}

/// Build the Axum router with all routes and shared state.
///
/// # Routes
///
/// | Method | Path                              | Handler                     |
/// |--------|-----------------------------------|-----------------------------|
/// | GET    | `/`                               | HTML dashboard              |
/// | GET    | `/api/v1/state`                   | Full orchestrator snapshot   |
/// | GET    | `/api/v1/{issue_identifier}`       | Single issue details        |
/// | POST   | `/api/v1/refresh`                 | Trigger poll+reconcile      |
/// | POST   | `/api/v1/approve/{approval_id}`   | Resolve a pending approval  |
pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(dashboard::handler::dashboard))
        .route("/api/v1/state", get(api::state::get_state))
        .route("/api/v1/refresh", post(api::refresh::post_refresh))
        .route(
            "/api/v1/approve/{approval_id}",
            post(api::approve::post_approve),
        )
        .route(
            "/api/v1/{issue_identifier}",
            get(api::issue::get_issue),
        )
        .with_state(state)
}

/// Start the HTTP server on the given port.
///
/// Binds to `127.0.0.1:<port>` and serves the provided router until the
/// process is terminated.
///
/// # Errors
///
/// Returns an error if the port is already in use or the listener cannot
/// be created.
pub async fn start_server(router: Router, port: u16) -> Result<(), anyhow::Error> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    tracing::info!("HTTP server listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, router).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_test_state() -> Arc<AppState> {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        Arc::new(AppState {
            orchestrator_tx: tx,
            snapshot_fn: Arc::new(|| {
                json!({
                    "generated_at": "2025-01-01T00:00:00Z",
                    "counts": { "running": 0, "retrying": 0 },
                    "running": [],
                    "retrying": [],
                    "codex_totals": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "total_tokens": 0,
                        "seconds_running": 0.0
                    },
                    "rate_limits": null
                })
            }),
            approval_queue: Arc::new(
                symphony_orchestrator::approval_queue::PendingApprovalQueue::new(),
            ),
        })
    }

    #[test]
    fn create_router_returns_valid_router() {
        let state = make_test_state();
        let _router = create_router(state);
        // Router construction should not panic.
    }
}
