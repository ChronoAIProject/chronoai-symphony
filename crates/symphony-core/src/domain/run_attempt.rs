use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Status of a run attempt through its lifecycle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunAttemptStatus {
    PreparingWorkspace,
    BuildingPrompt,
    LaunchingAgentProcess,
    InitializingSession,
    StreamingTurn,
    Finishing,
    Succeeded,
    Failed,
    TimedOut,
    Stalled,
    CanceledByReconciliation,
}

/// Tracks a single attempt to process an issue through the agent pipeline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunAttempt {
    pub issue_id: String,
    pub issue_identifier: String,

    /// Attempt number. `None` for the first run, `Some(n)` for retries (1-based).
    pub attempt: Option<u32>,

    pub workspace_path: PathBuf,
    pub started_at: DateTime<Utc>,
    pub status: RunAttemptStatus,
    pub error: Option<String>,
}
