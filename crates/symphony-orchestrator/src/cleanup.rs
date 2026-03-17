//! Startup terminal workspace cleanup.
//!
//! On orchestrator startup, removes workspace directories for issues
//! that are already in terminal states, freeing disk space from
//! previous runs.

use symphony_core::identifiers::normalize_state;
use symphony_tracker::traits::IssueTracker;
use symphony_workspace::manager::WorkspaceManager;
use tracing::{debug, info, warn};

/// Clean up workspaces for issues that are already in terminal states.
///
/// Queries the tracker for issues in terminal states and removes their
/// workspace directories. This is a best-effort operation: failures to
/// fetch issues or remove directories are logged and do not prevent
/// the orchestrator from starting.
pub async fn startup_terminal_cleanup(
    tracker: &dyn IssueTracker,
    workspace_manager: &WorkspaceManager,
    terminal_states: &[String],
) {
    info!(
        terminal_states = ?terminal_states,
        "starting terminal workspace cleanup"
    );

    let issues = match tracker
        .fetch_issues_by_states(terminal_states)
        .await
    {
        Ok(issues) => issues,
        Err(e) => {
            warn!(
                error = %e,
                "failed to fetch terminal issues for cleanup, skipping"
            );
            return;
        }
    };

    let mut cleaned = 0u32;
    for issue in &issues {
        let normalized = normalize_state(&issue.state);
        let is_terminal = terminal_states
            .iter()
            .any(|t| normalize_state(t) == normalized);

        if is_terminal {
            match workspace_manager.cleanup_workspace(&issue.identifier).await {
                Ok(()) => {
                    cleaned += 1;
                    info!(
                        issue_id = %issue.id,
                        identifier = %issue.identifier,
                        state = %issue.state,
                        "removed terminal workspace"
                    );
                }
                Err(e) => {
                    // Best effort: workspace may not exist on first run.
                    debug!(
                        issue_id = %issue.id,
                        identifier = %issue.identifier,
                        error = %e,
                        "skipped terminal workspace cleanup (not found)"
                    );
                }
            }
        }
    }

    info!(cleaned, "terminal workspace cleanup complete");
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use symphony_core::domain::Issue;
    use symphony_core::error::SymphonyError;
    use tempfile::TempDir;

    struct MockTracker {
        issues: Vec<Issue>,
    }

    #[async_trait]
    impl IssueTracker for MockTracker {
        async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>, SymphonyError> {
            Ok(vec![])
        }

        async fn fetch_issues_by_states(
            &self,
            _states: &[String],
        ) -> Result<Vec<Issue>, SymphonyError> {
            Ok(self.issues.clone())
        }

        async fn fetch_issue_states_by_ids(
            &self,
            _ids: &[String],
        ) -> Result<Vec<Issue>, SymphonyError> {
            Ok(vec![])
        }
    }

    fn make_issue(id: &str, identifier: &str, state: &str) -> Issue {
        Issue {
            id: id.to_string(),
            identifier: identifier.to_string(),
            title: format!("Issue {id}"),
            description: None,
            priority: None,
            state: state.to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        }
    }

    #[tokio::test]
    async fn cleans_terminal_workspaces() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceManager::new(
            tmp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            5000,
        );

        // Create workspaces.
        mgr.create_for_issue("issue-1").await.unwrap();
        mgr.create_for_issue("issue-2").await.unwrap();

        let tracker = MockTracker {
            issues: vec![
                make_issue("1", "issue-1", "Done"),
                make_issue("2", "issue-2", "In Progress"),
            ],
        };

        startup_terminal_cleanup(
            &tracker,
            &mgr,
            &["Done".to_string(), "Cancelled".to_string()],
        )
        .await;

        // issue-1 workspace should be removed (Done is terminal).
        assert!(!tmp.path().join("issue-1").exists());
        // issue-2 workspace should still exist (In Progress is not terminal).
        assert!(tmp.path().join("issue-2").exists());
    }

    #[tokio::test]
    async fn handles_tracker_fetch_failure() {
        struct FailingTracker;

        #[async_trait]
        impl IssueTracker for FailingTracker {
            async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>, SymphonyError> {
                Ok(vec![])
            }

            async fn fetch_issues_by_states(
                &self,
                _states: &[String],
            ) -> Result<Vec<Issue>, SymphonyError> {
                Err(SymphonyError::TrackerApiRequest {
                    detail: "network error".to_string(),
                })
            }

            async fn fetch_issue_states_by_ids(
                &self,
                _ids: &[String],
            ) -> Result<Vec<Issue>, SymphonyError> {
                Ok(vec![])
            }
        }

        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceManager::new(
            tmp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            5000,
        );

        // Should not panic.
        startup_terminal_cleanup(
            &FailingTracker,
            &mgr,
            &["Done".to_string()],
        )
        .await;
    }
}
