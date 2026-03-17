use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Represents an on-disk workspace created for a specific issue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Workspace {
    /// Absolute path to the workspace directory.
    pub path: PathBuf,

    /// Sanitized issue identifier used as the directory name.
    pub workspace_key: String,

    /// Whether this workspace was freshly created (true) or already existed (false).
    pub created_now: bool,
}
