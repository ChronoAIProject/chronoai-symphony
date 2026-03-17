use serde::{Deserialize, Serialize};

/// Aggregate token usage and timing across all codex sessions.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodexTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub seconds_running: f64,
}

impl Default for CodexTotals {
    fn default() -> Self {
        Self {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            seconds_running: 0.0,
        }
    }
}
