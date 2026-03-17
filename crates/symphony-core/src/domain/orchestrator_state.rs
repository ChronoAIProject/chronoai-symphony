use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::codex_totals::CodexTotals;
use super::issue::Issue;
use super::retry_entry::RetryEntry;

/// Per-issue tracking entry while an agent session is running.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunningEntry {
    pub identifier: String,
    pub issue: Issue,

    pub session_id: Option<String>,
    pub codex_app_server_pid: Option<String>,
    pub last_codex_message: Option<String>,
    pub last_codex_event: Option<String>,
    pub last_codex_timestamp: Option<DateTime<Utc>>,

    pub codex_input_tokens: u64,
    pub codex_output_tokens: u64,
    pub codex_total_tokens: u64,

    pub last_reported_input_tokens: u64,
    pub last_reported_output_tokens: u64,
    pub last_reported_total_tokens: u64,

    /// Retry attempt number if this is a retry, `None` for first run.
    pub retry_attempt: Option<u32>,

    pub started_at: DateTime<Utc>,
    pub turn_count: u32,
}

/// Top-level orchestrator state tracking all active, pending, and completed issues.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestratorState {
    pub poll_interval_ms: u64,
    pub max_concurrent_agents: u32,

    /// Currently running issue sessions, keyed by issue ID.
    pub running: HashMap<String, RunningEntry>,

    /// Issue IDs that have been claimed for processing but may not yet be running.
    pub claimed: HashSet<String>,

    /// Pending retry attempts, keyed by issue ID.
    pub retry_attempts: HashMap<String, RetryEntry>,

    /// Issue IDs that have completed successfully.
    pub completed: HashSet<String>,

    /// Aggregate token usage across all sessions.
    pub codex_totals: CodexTotals,

    /// Rate limit information from the codex API, if available.
    pub codex_rate_limits: Option<serde_json::Value>,
}
