//! Dispatch logic for selecting and prioritizing issues for agent processing.
//!
//! Implements eligibility checks and sorting to determine which issues
//! should be dispatched to agent workers, respecting concurrency limits,
//! state filters, and blocker dependencies.

use symphony_core::domain::{Issue, OrchestratorState, ServiceConfig};
use symphony_core::identifiers::normalize_state;
use tracing::debug;

use crate::concurrency::has_available_slots;

/// Check whether an issue is eligible for dispatch.
///
/// An issue is eligible when all of the following are true:
/// - It has non-empty id, identifier, title, and state fields.
/// - Its state is in the configured `tracker_active_states` (case-insensitive).
/// - Its state is NOT in the configured `tracker_terminal_states`.
/// - It is not in the running map.
/// - It is not in the claimed set.
/// - Global and per-state concurrency slots are available.
/// - If the state is "todo" (case-insensitive), no blockers have non-terminal
///   states.
pub fn is_dispatch_eligible(
    issue: &Issue,
    state: &OrchestratorState,
    config: &ServiceConfig,
) -> bool {
    // Basic field validation.
    if issue.id.is_empty()
        || issue.identifier.is_empty()
        || issue.title.is_empty()
        || issue.state.is_empty()
    {
        debug!(issue_id = %issue.id, "skipping issue: missing required fields");
        return false;
    }

    let normalized_state = normalize_state(&issue.state);

    // State must be in active_states.
    if !config
        .tracker_active_states
        .iter()
        .any(|s| normalize_state(s) == normalized_state)
    {
        debug!(
            issue_id = %issue.id,
            state = %issue.state,
            "skipping issue: state not in active_states"
        );
        return false;
    }

    // Skip "handoff" states where the agent should not work.
    // These are states where the agent has finished and is waiting for
    // human action (review, merge, etc.). The agent should only be
    // re-dispatched when the human moves it to "Rework" or back to an
    // implementation state.
    let handoff_states = ["human review", "human-review", "humanreview",
                          "merging", "blocked"];
    if handoff_states.iter().any(|h| normalize_state(h) == normalized_state) {
        debug!(
            issue_id = %issue.id,
            state = %issue.state,
            "skipping issue: in handoff state (waiting for human)"
        );
        return false;
    }

    // State must NOT be in terminal_states.
    if config
        .tracker_terminal_states
        .iter()
        .any(|s| normalize_state(s) == normalized_state)
    {
        debug!(
            issue_id = %issue.id,
            state = %issue.state,
            "skipping issue: state is terminal"
        );
        return false;
    }

    // Must not already be running.
    if state.running.contains_key(&issue.id) {
        debug!(issue_id = %issue.id, "skipping issue: already running");
        return false;
    }

    // Must not be claimed.
    if state.claimed.contains(&issue.id) {
        debug!(issue_id = %issue.id, "skipping issue: already claimed");
        return false;
    }

    // Concurrency limits.
    if !has_available_slots(
        state,
        &issue.state,
        &config.agent_max_concurrent_by_state,
    ) {
        debug!(issue_id = %issue.id, "skipping issue: no available slots");
        return false;
    }

    // Blocker rule: if state is "todo", check blockers.
    if normalized_state == "todo"
        && has_non_terminal_blockers(issue, &config.tracker_terminal_states)
    {
        debug!(
            issue_id = %issue.id,
            "skipping issue: has non-terminal blockers"
        );
        return false;
    }

    true
}

/// Sort issues for dispatch priority.
///
/// Sorting order:
/// 1. Priority ascending (`None` sorts last)
/// 2. Created-at oldest first (`None` sorts last)
/// 3. Identifier lexicographic
pub fn sort_for_dispatch(issues: &mut [Issue]) {
    issues.sort_by(|a, b| {
        // Priority: lower number = higher priority, None sorts last.
        let pri_a = a.priority.unwrap_or(i32::MAX);
        let pri_b = b.priority.unwrap_or(i32::MAX);
        let pri_cmp = pri_a.cmp(&pri_b);
        if pri_cmp != std::cmp::Ordering::Equal {
            return pri_cmp;
        }

        // Created-at: oldest first, None sorts last.
        let created_cmp = match (&a.created_at, &b.created_at) {
            (Some(ca), Some(cb)) => ca.cmp(cb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        if created_cmp != std::cmp::Ordering::Equal {
            return created_cmp;
        }

        // Identifier: lexicographic.
        a.identifier.cmp(&b.identifier)
    });
}

/// Check whether any of the issue's blockers have a non-terminal state.
fn has_non_terminal_blockers(issue: &Issue, terminal_states: &[String]) -> bool {
    issue.blocked_by.iter().any(|blocker| {
        match &blocker.state {
            Some(state) => {
                let normalized = normalize_state(state);
                !terminal_states
                    .iter()
                    .any(|t| normalize_state(t) == normalized)
            }
            // If blocker state is unknown, treat as non-terminal (conservative).
            None => true,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};
    use std::collections::{HashMap, HashSet};
    use symphony_core::domain::{BlockerRef, CodexTotals, HooksConfig, RunningEntry};

    fn empty_state() -> OrchestratorState {
        OrchestratorState {
            poll_interval_ms: 10_000,
            max_concurrent_agents: 5,
            running: HashMap::new(),
            claimed: HashSet::new(),
            retry_attempts: HashMap::new(),
            completed: HashSet::new(),
            codex_totals: CodexTotals::default(),
            codex_rate_limits: None,
        }
    }

    fn default_config() -> ServiceConfig {
        ServiceConfig {
            tracker_kind: "github".to_string(),
            tracker_endpoint: "https://api.github.com".to_string(),
            tracker_api_key: "test".to_string(),
            tracker_project_slug: "owner/repo".to_string(),
            tracker_active_states: vec!["Todo".to_string(), "In Progress".to_string()],
            tracker_terminal_states: vec!["Done".to_string(), "Cancelled".to_string()],
            polling_interval_ms: 30_000,
            workspace_root: std::path::PathBuf::from("/tmp/test"),
            hooks: HooksConfig {
                after_create: None,
                before_run: None,
                after_run: None,
                before_remove: None,
                timeout_ms: 60_000,
            },
            agent_max_concurrent: 5,
            agent_max_turns: 20,
            agent_max_retry_backoff_ms: 300_000,
            agent_max_concurrent_by_state: HashMap::new(),
            codex_command: "codex app-server".to_string(),
            codex_approval_policy: None,
            codex_thread_sandbox: None,
            codex_turn_sandbox_policy: None,
            codex_turn_timeout_ms: 3_600_000,
            codex_read_timeout_ms: 5_000,
            codex_stall_timeout_ms: 300_000,
            server_port: None,
        }
    }

    fn make_issue(id: &str, state: &str, priority: Option<i32>) -> Issue {
        Issue {
            id: id.to_string(),
            identifier: format!("#{id}"),
            title: format!("Issue {id}"),
            description: None,
            priority,
            state: state.to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    #[test]
    fn eligible_basic_issue() {
        let state = empty_state();
        let config = default_config();
        let issue = make_issue("1", "Todo", Some(1));
        assert!(is_dispatch_eligible(&issue, &state, &config));
    }

    #[test]
    fn ineligible_empty_id() {
        let state = empty_state();
        let config = default_config();
        let mut issue = make_issue("1", "Todo", Some(1));
        issue.id = String::new();
        assert!(!is_dispatch_eligible(&issue, &state, &config));
    }

    #[test]
    fn ineligible_terminal_state() {
        let state = empty_state();
        let config = default_config();
        let issue = make_issue("1", "Done", Some(1));
        assert!(!is_dispatch_eligible(&issue, &state, &config));
    }

    #[test]
    fn ineligible_non_active_state() {
        let state = empty_state();
        let config = default_config();
        let issue = make_issue("1", "Backlog", Some(1));
        assert!(!is_dispatch_eligible(&issue, &state, &config));
    }

    #[test]
    fn ineligible_already_running() {
        let mut state = empty_state();
        let config = default_config();
        let issue = make_issue("1", "Todo", Some(1));

        state.running.insert(
            "1".to_string(),
            RunningEntry {
                identifier: "#1".to_string(),
                issue: issue.clone(),
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
                started_at: Utc::now(),
                turn_count: 0,
            },
        );

        assert!(!is_dispatch_eligible(&issue, &state, &config));
    }

    #[test]
    fn ineligible_claimed() {
        let mut state = empty_state();
        let config = default_config();
        let issue = make_issue("1", "Todo", Some(1));
        state.claimed.insert("1".to_string());
        assert!(!is_dispatch_eligible(&issue, &state, &config));
    }

    #[test]
    fn ineligible_blocked_by_non_terminal() {
        let state = empty_state();
        let config = default_config();
        let mut issue = make_issue("1", "Todo", Some(1));
        issue.blocked_by.push(BlockerRef {
            id: Some("2".to_string()),
            identifier: Some("#2".to_string()),
            state: Some("In Progress".to_string()),
        });
        assert!(!is_dispatch_eligible(&issue, &state, &config));
    }

    #[test]
    fn eligible_blocked_by_terminal() {
        let state = empty_state();
        let config = default_config();
        let mut issue = make_issue("1", "Todo", Some(1));
        issue.blocked_by.push(BlockerRef {
            id: Some("2".to_string()),
            identifier: Some("#2".to_string()),
            state: Some("Done".to_string()),
        });
        assert!(is_dispatch_eligible(&issue, &state, &config));
    }

    #[test]
    fn sort_by_priority_ascending() {
        let mut issues = vec![
            make_issue("3", "Todo", Some(3)),
            make_issue("1", "Todo", Some(1)),
            make_issue("2", "Todo", Some(2)),
        ];
        sort_for_dispatch(&mut issues);
        assert_eq!(issues[0].id, "1");
        assert_eq!(issues[1].id, "2");
        assert_eq!(issues[2].id, "3");
    }

    #[test]
    fn sort_none_priority_last() {
        let mut issues = vec![
            make_issue("2", "Todo", None),
            make_issue("1", "Todo", Some(1)),
        ];
        sort_for_dispatch(&mut issues);
        assert_eq!(issues[0].id, "1");
        assert_eq!(issues[1].id, "2");
    }

    #[test]
    fn sort_by_created_at_when_priority_equal() {
        let mut issue_a = make_issue("a", "Todo", Some(1));
        issue_a.created_at = Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap());
        let mut issue_b = make_issue("b", "Todo", Some(1));
        issue_b.created_at = Some(Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap());

        let mut issues = vec![issue_b, issue_a];
        sort_for_dispatch(&mut issues);
        assert_eq!(issues[0].id, "a");
        assert_eq!(issues[1].id, "b");
    }

    #[test]
    fn sort_by_identifier_as_tiebreaker() {
        let mut issues = vec![
            make_issue("b", "Todo", Some(1)),
            make_issue("a", "Todo", Some(1)),
        ];
        sort_for_dispatch(&mut issues);
        assert_eq!(issues[0].id, "a");
        assert_eq!(issues[1].id, "b");
    }

    #[test]
    fn case_insensitive_state_matching() {
        let state = empty_state();
        let config = default_config();
        let issue = make_issue("1", "todo", Some(1));
        assert!(is_dispatch_eligible(&issue, &state, &config));
    }
}
