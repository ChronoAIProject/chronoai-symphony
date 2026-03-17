use serde::{Deserialize, Serialize};

/// Tracks a pending retry for a failed issue run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetryEntry {
    pub issue_id: String,
    pub identifier: String,

    /// 1-based attempt number.
    pub attempt: u32,

    /// Monotonic timestamp (in milliseconds) when this retry becomes eligible.
    pub due_at_ms: u64,

    /// Error message from the previous failed attempt.
    pub error: Option<String>,
}
