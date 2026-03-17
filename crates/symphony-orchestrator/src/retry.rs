//! Retry queue and exponential backoff for failed and continuation runs.
//!
//! Manages scheduling of retry attempts with configurable backoff, including
//! immediate retries for continuation and exponential backoff for failures.

use std::time::{Duration, Instant};

use symphony_core::domain::{OrchestratorState, RetryEntry};
use tracing::info;

/// Schedule a continuation retry with a short fixed delay.
///
/// Used when a turn completes but the issue is still in an active state,
/// requiring another pass. Returns a `tokio::time::Sleep` future that
/// resolves when the retry is due.
pub fn schedule_continuation_retry(
    state: &mut OrchestratorState,
    issue_id: String,
    identifier: String,
) -> tokio::time::Sleep {
    let delay_ms: u64 = 1000;
    let now = Instant::now();
    let due_at_ms = now.elapsed().as_millis() as u64 + delay_ms;

    info!(
        issue_id = %issue_id,
        delay_ms,
        "scheduling continuation retry"
    );

    state.retry_attempts.insert(
        issue_id,
        RetryEntry {
            issue_id: String::new(), // Will be set from the key.
            identifier,
            attempt: 1,
            due_at_ms,
            error: None,
        },
    );

    // Fix up the issue_id inside the entry.
    if let Some(entry) = state.retry_attempts.values_mut().last() {
        entry.issue_id = entry.identifier.clone();
    }

    tokio::time::sleep(Duration::from_millis(delay_ms))
}

/// Schedule a failure retry with exponential backoff.
///
/// The delay doubles with each attempt: `min(10000 * 2^(attempt-1), max_backoff_ms)`.
/// Returns a `tokio::time::Sleep` future that resolves when the retry is due.
pub fn schedule_failure_retry(
    state: &mut OrchestratorState,
    issue_id: String,
    identifier: String,
    attempt: u32,
    error: String,
    max_backoff_ms: u64,
) -> tokio::time::Sleep {
    let delay_ms = compute_backoff_ms(attempt, max_backoff_ms);
    let now = Instant::now();
    let due_at_ms = now.elapsed().as_millis() as u64 + delay_ms;

    info!(
        issue_id = %issue_id,
        attempt,
        delay_ms,
        "scheduling failure retry"
    );

    state.retry_attempts.insert(
        issue_id.clone(),
        RetryEntry {
            issue_id,
            identifier,
            attempt,
            due_at_ms,
            error: Some(error),
        },
    );

    tokio::time::sleep(Duration::from_millis(delay_ms))
}

/// Compute the backoff delay in milliseconds for a given attempt number.
///
/// Formula: `min(10000 * 2^(attempt - 1), max_backoff_ms)`
///
/// - Attempt 1: 10,000 ms (10 seconds)
/// - Attempt 2: 20,000 ms (20 seconds)
/// - Attempt 3: 40,000 ms (40 seconds)
/// - ...capped at `max_backoff_ms`
pub fn compute_backoff_ms(attempt: u32, max_backoff_ms: u64) -> u64 {
    let base: u64 = 10_000;
    let exponent = attempt.saturating_sub(1);
    let backoff = base.saturating_mul(2u64.saturating_pow(exponent));
    backoff.min(max_backoff_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_attempt_1() {
        assert_eq!(compute_backoff_ms(1, 600_000), 10_000);
    }

    #[test]
    fn backoff_attempt_2() {
        assert_eq!(compute_backoff_ms(2, 600_000), 20_000);
    }

    #[test]
    fn backoff_attempt_3() {
        assert_eq!(compute_backoff_ms(3, 600_000), 40_000);
    }

    #[test]
    fn backoff_attempt_4() {
        assert_eq!(compute_backoff_ms(4, 600_000), 80_000);
    }

    #[test]
    fn backoff_capped_at_max() {
        assert_eq!(compute_backoff_ms(10, 60_000), 60_000);
    }

    #[test]
    fn backoff_attempt_0_treated_as_1() {
        // attempt 0 -> exponent = 0 - 1 = saturates to 0 -> 10000 * 1 = 10000
        assert_eq!(compute_backoff_ms(0, 600_000), 10_000);
    }

    #[test]
    fn backoff_very_large_attempt_does_not_overflow() {
        let result = compute_backoff_ms(100, 300_000);
        assert_eq!(result, 300_000);
    }

    #[test]
    fn backoff_max_zero_clamps_to_zero() {
        assert_eq!(compute_backoff_ms(1, 0), 0);
    }
}
