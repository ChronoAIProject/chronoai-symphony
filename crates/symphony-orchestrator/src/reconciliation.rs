//! Active run reconciliation for detecting stalls and state drift.
//!
//! Periodically checks running agent sessions against the issue tracker
//! to detect terminal state transitions, and monitors for stalled sessions
//! that have stopped producing output.

use chrono::Utc;
use symphony_core::domain::{OrchestratorState, ServiceConfig};
use symphony_core::identifiers::normalize_state;
use symphony_tracker::traits::IssueTracker;
use symphony_workspace::manager::WorkspaceManager;
use tracing::{info, warn};

/// Actions that the orchestrator should take based on reconciliation results.
#[derive(Debug, Clone)]
pub enum ReconciliationAction {
    /// Terminate the worker and clean up its workspace.
    TerminateAndCleanup { issue_id: String },

    /// Terminate the worker but do not clean up (issue may be in an
    /// unexpected state).
    TerminateNoCleanup { issue_id: String },

    /// Update the issue snapshot in the running entry.
    UpdateSnapshot { issue_id: String, new_state: String },

    /// A running session has not produced output within the stall timeout.
    StallDetected { issue_id: String },
}

/// Reconcile running issues against the tracker and detect stalls.
///
/// This performs two checks:
/// - **Part A (Stall detection)**: For each running issue, checks if the
///   elapsed time since the last codex message exceeds the stall timeout.
/// - **Part B (Tracker state refresh)**: Fetches current states from the
///   tracker and determines if any running issues have transitioned to
///   terminal or unexpected states.
pub async fn reconcile_running_issues(
    state: &OrchestratorState,
    tracker: &dyn IssueTracker,
    _workspace_manager: &WorkspaceManager,
    config: &ServiceConfig,
) -> Vec<ReconciliationAction> {
    let mut actions = Vec::new();

    // Part A: Stall detection.
    let now = Utc::now();
    for (issue_id, entry) in &state.running {
        if entry.stop_requested_at.is_some() {
            continue;
        }

        let stall_timeout_ms = entry.stall_timeout_ms;
        if stall_timeout_ms > 0 {
            let reference_time = entry.last_codex_timestamp.unwrap_or(entry.started_at);
            let elapsed_ms = (now - reference_time).num_milliseconds().max(0) as u64;

            if elapsed_ms > stall_timeout_ms as u64 {
                info!(
                    issue_id = %issue_id,
                    elapsed_ms,
                    stall_timeout_ms,
                    "stall detected"
                );
                actions.push(ReconciliationAction::StallDetected {
                    issue_id: issue_id.clone(),
                });
            }
        }
    }

    // Part B: Tracker state refresh.
    if state.running.is_empty() {
        return actions;
    }

    let running_keys: Vec<String> = state.running.keys().cloned().collect();

    // Strip ":role" suffix from compound keys (e.g., "#82:triage" -> "#82")
    // for tracker lookups. The tracker only knows raw issue IDs.
    let raw_issue_ids: Vec<String> = running_keys
        .iter()
        .map(|key| key.split(':').next().unwrap_or(key).to_string())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    match tracker.fetch_issue_states_by_ids(&raw_issue_ids).await {
        Ok(current_issues) => {
            // Build a lookup from raw issue ID to current state.
            let state_lookup: std::collections::HashMap<String, String> = current_issues
                .iter()
                .map(|issue| (issue.id.clone(), issue.state.clone()))
                .collect();

            for issue_id in &running_keys {
                if state
                    .running
                    .get(issue_id)
                    .and_then(|entry| entry.stop_requested_at)
                    .is_some()
                {
                    continue;
                }

                // Extract the raw issue ID from the compound key for lookup.
                let raw_id = issue_id.split(':').next().unwrap_or(issue_id);
                match state_lookup.get(raw_id) {
                    Some(current_state) => {
                        let normalized = normalize_state(current_state);

                        if config
                            .tracker_terminal_states
                            .iter()
                            .any(|t| normalize_state(t) == normalized)
                        {
                            info!(
                                issue_id = %issue_id,
                                state = %current_state,
                                "issue reached terminal state, terminating worker"
                            );
                            actions.push(ReconciliationAction::TerminateAndCleanup {
                                issue_id: issue_id.clone(),
                            });
                        } else if config
                            .tracker_active_states
                            .iter()
                            .any(|a| normalize_state(a) == normalized)
                        {
                            actions.push(ReconciliationAction::UpdateSnapshot {
                                issue_id: issue_id.clone(),
                                new_state: current_state.clone(),
                            });
                        } else {
                            warn!(
                                issue_id = %issue_id,
                                state = %current_state,
                                "issue in unexpected state, terminating without cleanup"
                            );
                            actions.push(ReconciliationAction::TerminateNoCleanup {
                                issue_id: issue_id.clone(),
                            });
                        }
                    }
                    None => {
                        warn!(
                            issue_id = %issue_id,
                            "issue not found in tracker response, terminating without cleanup"
                        );
                        actions.push(ReconciliationAction::TerminateNoCleanup {
                            issue_id: issue_id.clone(),
                        });
                    }
                }
            }
        }
        Err(e) => {
            // Refresh failure: keep workers running, just log.
            warn!(
                error = %e,
                "failed to refresh issue states from tracker, keeping workers running"
            );
        }
    }

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Duration;
    use std::collections::{HashMap, HashSet};
    use std::path::PathBuf;
    use symphony_core::domain::config::{AgentProfileConfig, AgentType, HooksConfig};
    use symphony_core::domain::{CodexTotals, Issue, RunningEntry};
    use symphony_core::error::SymphonyError;

    struct StaticTracker;

    #[async_trait]
    impl IssueTracker for StaticTracker {
        async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>, SymphonyError> {
            Ok(vec![])
        }

        async fn fetch_issues_by_states(
            &self,
            _states: &[String],
        ) -> Result<Vec<Issue>, SymphonyError> {
            Ok(vec![])
        }

        async fn fetch_issue_states_by_ids(
            &self,
            _ids: &[String],
        ) -> Result<Vec<Issue>, SymphonyError> {
            Ok(vec![])
        }
    }

    fn config() -> ServiceConfig {
        let default_profile = AgentProfileConfig {
            agent_type: AgentType::Codex,
            command: "codex".to_string(),
            approval_policy: None,
            thread_sandbox: None,
            turn_sandbox_policy: None,
            turn_timeout_ms: 3_600_000,
            read_timeout_ms: 5_000,
            stall_timeout_ms: 300_000,
            model: None,
            reasoning_effort: None,
            network_access: true,
            max_turns: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
        };
        let mut agent_profiles = HashMap::new();
        agent_profiles.insert("codex".to_string(), default_profile);

        ServiceConfig {
            tracker_kind: "github".to_string(),
            tracker_endpoint: "https://api.github.com".to_string(),
            tracker_api_key: "token".to_string(),
            tracker_project_slug: "owner/repo".to_string(),
            tracker_active_states: vec!["Todo".to_string()],
            tracker_terminal_states: vec!["Done".to_string()],
            github_app_id: None,
            github_app_installation_id: None,
            github_app_private_key_path: None,
            polling_interval_ms: 30_000,
            workspace_root: PathBuf::from("/tmp/workspaces"),
            git_user_name: None,
            git_user_email: None,
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
            agent_require_label: None,
            agent_profiles,
            default_agent: "codex".to_string(),
            agent_by_state: HashMap::new(),
            codex_command: "codex".to_string(),
            codex_approval_policy: None,
            codex_thread_sandbox: None,
            codex_turn_sandbox_policy: None,
            codex_turn_timeout_ms: 3_600_000,
            codex_read_timeout_ms: 5_000,
            codex_stall_timeout_ms: 300_000,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_network_access: true,
            codex_auto_merge: false,
            pipeline_stages: vec![],
            prompt_state_instructions: HashMap::new(),
            prompt_role_instructions: HashMap::new(),
            server_port: None,
        }
    }

    fn issue(id: &str) -> Issue {
        Issue {
            id: id.to_string(),
            identifier: format!("#{id}"),
            title: "Test".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    fn running_entry(id: &str, stall_timeout_ms: i64, stop_requested: bool) -> RunningEntry {
        RunningEntry {
            identifier: format!("#{id}"),
            issue: issue(id),
            agent_type: "codex".to_string(),
            stall_timeout_ms,
            session_id: None,
            codex_app_server_pid: None,
            last_codex_message: None,
            last_codex_event: None,
            last_codex_timestamp: Some(Utc::now() - Duration::minutes(15)),
            stop_requested_at: stop_requested.then(Utc::now),
            codex_input_tokens: 0,
            codex_output_tokens: 0,
            codex_total_tokens: 0,
            last_reported_input_tokens: 0,
            last_reported_output_tokens: 0,
            last_reported_total_tokens: 0,
            retry_attempt: None,
            stage_role: None,
            dispatched_state: "Todo".to_string(),
            started_at: Utc::now() - Duration::minutes(20),
            turn_count: 0,
        }
    }

    fn orchestrator_state(entry: RunningEntry) -> OrchestratorState {
        let mut running = HashMap::new();
        running.insert(entry.issue.id.clone(), entry);
        OrchestratorState {
            poll_interval_ms: 30_000,
            max_concurrent_agents: 5,
            running,
            claimed: HashSet::new(),
            retry_attempts: HashMap::new(),
            completed: HashSet::new(),
            codex_totals: CodexTotals::default(),
            codex_rate_limits: None,
        }
    }

    #[tokio::test]
    async fn stall_detection_uses_running_entry_timeout() {
        let state = orchestrator_state(running_entry("1", 1_000, false));
        let tracker = StaticTracker;
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace_manager =
            WorkspaceManager::new(tmp.path().to_path_buf(), None, None, None, None, 5_000);

        let actions =
            reconcile_running_issues(&state, &tracker, &workspace_manager, &config()).await;

        assert!(actions.iter().any(|action| {
            matches!(action, ReconciliationAction::StallDetected { issue_id } if issue_id == "1")
        }));
    }

    #[tokio::test]
    async fn stall_detection_skips_entries_already_stopping() {
        let state = orchestrator_state(running_entry("1", 1_000, true));
        let tracker = StaticTracker;
        let tmp = tempfile::TempDir::new().unwrap();
        let workspace_manager =
            WorkspaceManager::new(tmp.path().to_path_buf(), None, None, None, None, 5_000);

        let actions =
            reconcile_running_issues(&state, &tracker, &workspace_manager, &config()).await;

        assert!(actions.is_empty());
    }
}
