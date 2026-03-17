//! Runtime events emitted by the agent process to the orchestrator.
//!
//! These events provide visibility into the agent session lifecycle,
//! including startup, turn completion, token usage, and error conditions.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Events emitted during an agent session for orchestrator consumption.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// Agent session successfully started with IDs assigned.
    SessionStarted {
        session_id: String,
        thread_id: String,
        turn_id: String,
        pid: Option<String>,
        timestamp: DateTime<Utc>,
    },

    /// Agent process failed to start or handshake failed.
    StartupFailed {
        error: String,
        timestamp: DateTime<Utc>,
    },

    /// A turn completed successfully.
    TurnCompleted {
        timestamp: DateTime<Utc>,
        usage: Option<TokenUsage>,
    },

    /// A turn ended with an error.
    TurnFailed {
        error: String,
        timestamp: DateTime<Utc>,
    },

    /// A turn was cancelled.
    TurnCancelled {
        timestamp: DateTime<Utc>,
    },

    /// The agent requires user input to continue.
    TurnInputRequired {
        timestamp: DateTime<Utc>,
    },

    /// An approval request was received from the agent process.
    ApprovalRequested {
        approval_id: String,
        method: String,
        timestamp: DateTime<Utc>,
    },

    /// An approval request was auto-approved by the agent runner.
    ApprovalAutoApproved {
        timestamp: DateTime<Utc>,
    },

    /// The agent attempted to call an unsupported tool.
    UnsupportedToolCall {
        tool_name: String,
        timestamp: DateTime<Utc>,
    },

    /// A notification message from the agent process.
    Notification {
        message: String,
        timestamp: DateTime<Utc>,
    },

    /// Updated token usage counters.
    TokenUsageUpdate {
        usage: TokenUsage,
        timestamp: DateTime<Utc>,
    },

    /// Updated rate limit information from the API.
    RateLimitUpdate {
        rate_limits: Value,
        timestamp: DateTime<Utc>,
    },

    /// A message that does not match any known event pattern.
    OtherMessage {
        raw: Value,
        timestamp: DateTime<Utc>,
    },

    /// A line that could not be parsed as valid JSON.
    Malformed {
        raw: String,
        timestamp: DateTime<Utc>,
    },
}

/// Aggregate token usage counters for a session or turn.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    /// Create a new `TokenUsage` with the given counts.
    pub fn new(input_tokens: u64, output_tokens: u64, total_tokens: u64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            total_tokens,
        }
    }

    /// Create a `TokenUsage` with all counters at zero.
    pub fn zero() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_default_is_zero() {
        let usage = TokenUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.total_tokens, 0);
    }

    #[test]
    fn token_usage_new_sets_values() {
        let usage = TokenUsage::new(100, 200, 300);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.total_tokens, 300);
    }

    #[test]
    fn agent_event_session_started_carries_ids() {
        let event = AgentEvent::SessionStarted {
            session_id: "s1".to_string(),
            thread_id: "t1".to_string(),
            turn_id: "u1".to_string(),
            pid: Some("1234".to_string()),
            timestamp: Utc::now(),
        };
        match event {
            AgentEvent::SessionStarted { session_id, .. } => {
                assert_eq!(session_id, "s1");
            }
            _ => panic!("unexpected variant"),
        }
    }
}
