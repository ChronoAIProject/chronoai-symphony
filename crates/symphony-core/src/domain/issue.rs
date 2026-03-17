use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A reference to a blocking issue that prevents work on the current issue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockerRef {
    pub id: Option<String>,
    pub identifier: Option<String>,
    pub state: Option<String>,
}

/// Normalized issue record, tracker-agnostic representation of a work item.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Issue {
    /// Stable tracker-internal ID.
    pub id: String,

    /// Human-readable key (e.g., `#123`).
    pub identifier: String,

    pub title: String,
    pub description: Option<String>,

    /// Priority value where lower numbers indicate higher priority.
    pub priority: Option<i32>,

    /// Current tracker state (e.g., "Todo", "In Progress").
    pub state: String,

    /// Suggested branch name for this issue.
    pub branch_name: Option<String>,

    /// URL to the issue in the tracker.
    pub url: Option<String>,

    /// Labels attached to the issue, normalized to lowercase.
    pub labels: Vec<String>,

    /// Issues that block this one.
    pub blocked_by: Vec<BlockerRef>,

    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}
