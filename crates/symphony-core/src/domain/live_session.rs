use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Tracks a live codex agent session within a run attempt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LiveSession {
    /// Composite session identifier: `<thread_id>-<turn_id>`.
    pub session_id: String,

    pub thread_id: String,
    pub turn_id: String,

    /// PID of the codex app-server process.
    pub codex_app_server_pid: Option<String>,

    /// Most recent event type received from codex.
    pub last_codex_event: Option<String>,

    /// Timestamp of the most recent codex event.
    pub last_codex_timestamp: Option<DateTime<Utc>>,

    /// Most recent message content from codex.
    pub last_codex_message: Option<String>,

    // Token usage for the current session.
    pub codex_input_tokens: u64,
    pub codex_output_tokens: u64,
    pub codex_total_tokens: u64,

    // Token usage as of the last status report.
    pub last_reported_input_tokens: u64,
    pub last_reported_output_tokens: u64,
    pub last_reported_total_tokens: u64,

    /// Number of turns completed in this session.
    pub turn_count: u32,
}
