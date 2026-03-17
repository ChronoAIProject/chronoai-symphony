use async_trait::async_trait;

use symphony_core::domain::Issue;
use symphony_core::error::SymphonyError;

/// Abstraction over issue-tracking systems (GitHub, Linear, Jira, etc.).
///
/// Implementations fetch and normalize issues from a specific tracker backend
/// into the unified `Issue` domain type.
#[async_trait]
pub trait IssueTracker: Send + Sync {
    /// Fetch open issues that are candidates for agent processing.
    ///
    /// Typically returns issues in active states (e.g., "Todo", "In Progress")
    /// that have not yet been completed.
    async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>, SymphonyError>;

    /// Fetch issues filtered by the given state names.
    ///
    /// States are matched case-insensitively against the tracker's native
    /// state or label representation.
    async fn fetch_issues_by_states(
        &self,
        states: &[String],
    ) -> Result<Vec<Issue>, SymphonyError>;

    /// Fetch current state information for issues identified by their IDs.
    ///
    /// Returns minimal `Issue` records with at least `id`, `identifier`, and
    /// `state` populated. Used for polling state changes on known issues.
    async fn fetch_issue_states_by_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<Issue>, SymphonyError>;
}
