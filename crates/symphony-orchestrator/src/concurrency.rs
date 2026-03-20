//! Concurrency control for agent dispatch.
//!
//! Implements global and per-state slot tracking to ensure the orchestrator
//! does not exceed configured limits on concurrent agent sessions.

use std::collections::HashMap;

use symphony_core::domain::OrchestratorState;
use symphony_core::identifiers::normalize_state;

/// Calculate the number of globally available agent slots.
///
/// Returns `max(max_concurrent_agents - running_count, 0)`.
pub fn available_global_slots(state: &OrchestratorState) -> u32 {
    let running = state.running.len() as u32;
    state.max_concurrent_agents.saturating_sub(running)
}

/// Calculate the number of available slots for a specific issue state.
///
/// If a per-state limit exists in `by_state_limits`, counts how many
/// running issues share that state and returns the remaining capacity.
/// If no per-state limit is configured, falls back to the global slot count.
pub fn available_state_slots(
    state: &OrchestratorState,
    issue_state: &str,
    by_state_limits: &HashMap<String, u32>,
) -> u32 {
    let normalized = normalize_state(issue_state);

    match by_state_limits.get(&normalized) {
        Some(&limit) => {
            let running_in_state = state
                .running
                .values()
                .filter(|entry| normalize_state(&entry.issue.state) == normalized)
                .count() as u32;
            limit.saturating_sub(running_in_state)
        }
        None => available_global_slots(state),
    }
}

/// Check whether there are available slots for dispatching an issue
/// in the given state.
///
/// Returns `true` only if both global and per-state slots are available.
pub fn has_available_slots(
    state: &OrchestratorState,
    issue_state: &str,
    by_state_limits: &HashMap<String, u32>,
) -> bool {
    available_global_slots(state) > 0
        && available_state_slots(state, issue_state, by_state_limits) > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::collections::HashSet;
    use symphony_core::domain::{CodexTotals, Issue, RunningEntry};

    fn empty_state(max_concurrent: u32) -> OrchestratorState {
        OrchestratorState {
            poll_interval_ms: 10_000,
            max_concurrent_agents: max_concurrent,
            running: HashMap::new(),
            claimed: HashSet::new(),
            retry_attempts: HashMap::new(),
            completed: HashSet::new(),
            codex_totals: CodexTotals::default(),
            codex_rate_limits: None,
        }
    }

    fn make_running_entry(issue_state: &str) -> RunningEntry {
        RunningEntry {
            identifier: "test".to_string(),
            issue: Issue {
                id: "1".to_string(),
                identifier: "#1".to_string(),
                title: "Test".to_string(),
                description: None,
                priority: None,
                state: issue_state.to_string(),
                branch_name: None,
                url: None,
                labels: vec![],
                blocked_by: vec![],
                created_at: None,
                updated_at: None,
            },
            agent_type: "codex".to_string(),
            session_id: None,
            codex_app_server_pid: None,
            last_codex_message: None,
            last_codex_event: None,
            last_codex_timestamp: None,
            codex_input_tokens: 0,
            codex_output_tokens: 0,
            codex_total_tokens: 0,
            last_reported_input_tokens: 0,
            last_reported_output_tokens: 0,
            last_reported_total_tokens: 0,
            retry_attempt: None,
            stage_role: None,
            started_at: Utc::now(),
            turn_count: 0,
        }
    }

    #[test]
    fn global_slots_empty_state() {
        let state = empty_state(5);
        assert_eq!(available_global_slots(&state), 5);
    }

    #[test]
    fn global_slots_some_running() {
        let mut state = empty_state(3);
        state
            .running
            .insert("a".to_string(), make_running_entry("todo"));
        assert_eq!(available_global_slots(&state), 2);
    }

    #[test]
    fn global_slots_at_capacity() {
        let mut state = empty_state(1);
        state
            .running
            .insert("a".to_string(), make_running_entry("todo"));
        assert_eq!(available_global_slots(&state), 0);
    }

    #[test]
    fn global_slots_over_capacity() {
        let mut state = empty_state(0);
        state
            .running
            .insert("a".to_string(), make_running_entry("todo"));
        assert_eq!(available_global_slots(&state), 0);
    }

    #[test]
    fn state_slots_no_limit_falls_back_to_global() {
        let state = empty_state(5);
        let limits = HashMap::new();
        assert_eq!(available_state_slots(&state, "todo", &limits), 5);
    }

    #[test]
    fn state_slots_with_limit() {
        let mut state = empty_state(10);
        state
            .running
            .insert("a".to_string(), make_running_entry("Todo"));
        let mut limits = HashMap::new();
        limits.insert("todo".to_string(), 2);
        assert_eq!(available_state_slots(&state, "Todo", &limits), 1);
    }

    #[test]
    fn state_slots_at_state_capacity() {
        let mut state = empty_state(10);
        state
            .running
            .insert("a".to_string(), make_running_entry("todo"));
        let mut limits = HashMap::new();
        limits.insert("todo".to_string(), 1);
        assert_eq!(available_state_slots(&state, "todo", &limits), 0);
    }

    #[test]
    fn has_slots_both_available() {
        let state = empty_state(5);
        let limits = HashMap::new();
        assert!(has_available_slots(&state, "todo", &limits));
    }

    #[test]
    fn has_slots_global_full() {
        let mut state = empty_state(1);
        state
            .running
            .insert("a".to_string(), make_running_entry("todo"));
        let limits = HashMap::new();
        assert!(!has_available_slots(&state, "todo", &limits));
    }

    #[test]
    fn has_slots_state_full() {
        let mut state = empty_state(10);
        state
            .running
            .insert("a".to_string(), make_running_entry("review"));
        let mut limits = HashMap::new();
        limits.insert("review".to_string(), 1);
        assert!(!has_available_slots(&state, "review", &limits));
    }
}
