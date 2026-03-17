//! Orchestrator event types for the internal event loop.
//!
//! The orchestrator processes these events to coordinate worker lifecycle,
//! handle agent updates, manage retries, and respond to configuration changes.

use symphony_agent::protocol::events::AgentEvent;
use symphony_core::domain::ServiceConfig;

/// Events processed by the orchestrator's main event loop.
#[derive(Debug)]
pub enum OrchestratorEvent {
    /// Periodic tick for polling, reconciliation, and dispatch.
    Tick,

    /// A worker task has exited (normally or with an error).
    WorkerExited {
        issue_id: String,
        reason: WorkerExitReason,
    },

    /// An agent event was received from a running worker.
    CodexUpdate {
        issue_id: String,
        event: AgentEvent,
    },

    /// A retry timer has fired and the issue is eligible for re-dispatch.
    RetryTimerFired {
        issue_id: String,
    },

    /// The workflow configuration has been reloaded from disk.
    WorkflowReloaded {
        config: ServiceConfig,
        prompt_template: String,
    },

    /// An external request to trigger an immediate reconciliation cycle.
    RefreshRequested,

    /// Graceful shutdown signal.
    Shutdown,
}

/// Reason a worker task exited.
#[derive(Debug, Clone)]
pub enum WorkerExitReason {
    /// The worker completed its work successfully.
    Normal,
    /// The worker encountered an error and could not continue.
    Abnormal(String),
}
