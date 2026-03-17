//! Timeout configuration for agent sessions.
//!
//! Encapsulates the various timeout durations used during the agent
//! lifecycle: read timeouts for individual message exchanges, turn
//! timeouts for overall turn duration, and optional stall detection.

use std::time::Duration;

/// Default read timeout: 120 seconds.
const DEFAULT_READ_TIMEOUT_MS: u64 = 120_000;

/// Default turn timeout: 30 minutes.
const DEFAULT_TURN_TIMEOUT_MS: u64 = 1_800_000;

/// Timeout configuration for agent session operations.
#[derive(Debug, Clone)]
pub struct TimeoutConfig {
    /// Maximum time to wait for a single message read from the agent process.
    pub read_timeout: Duration,

    /// Maximum time allowed for a complete turn (prompt to completion).
    pub turn_timeout: Duration,

    /// Optional stall detection timeout. If no messages are received within
    /// this duration during a turn, the session is considered stalled.
    /// `None` if stall detection is disabled (stall_timeout_ms <= 0).
    pub stall_timeout: Option<Duration>,
}

impl TimeoutConfig {
    /// Create a new timeout configuration from millisecond values.
    ///
    /// A `stall_timeout_ms` value of zero or negative disables stall detection.
    pub fn new(read_timeout_ms: u64, turn_timeout_ms: u64, stall_timeout_ms: i64) -> Self {
        let stall_timeout = if stall_timeout_ms > 0 {
            Some(Duration::from_millis(stall_timeout_ms as u64))
        } else {
            None
        };

        Self {
            read_timeout: Duration::from_millis(read_timeout_ms),
            turn_timeout: Duration::from_millis(turn_timeout_ms),
            stall_timeout,
        }
    }
}

impl Default for TimeoutConfig {
    fn default() -> Self {
        Self::new(DEFAULT_READ_TIMEOUT_MS, DEFAULT_TURN_TIMEOUT_MS, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_with_positive_stall_timeout() {
        let config = TimeoutConfig::new(5000, 60_000, 30_000);
        assert_eq!(config.read_timeout, Duration::from_millis(5000));
        assert_eq!(config.turn_timeout, Duration::from_millis(60_000));
        assert_eq!(config.stall_timeout, Some(Duration::from_millis(30_000)));
    }

    #[test]
    fn new_with_zero_stall_timeout_disables() {
        let config = TimeoutConfig::new(5000, 60_000, 0);
        assert!(config.stall_timeout.is_none());
    }

    #[test]
    fn new_with_negative_stall_timeout_disables() {
        let config = TimeoutConfig::new(5000, 60_000, -1);
        assert!(config.stall_timeout.is_none());
    }

    #[test]
    fn default_values() {
        let config = TimeoutConfig::default();
        assert_eq!(config.read_timeout, Duration::from_millis(120_000));
        assert_eq!(config.turn_timeout, Duration::from_millis(1_800_000));
        assert!(config.stall_timeout.is_none());
    }
}
