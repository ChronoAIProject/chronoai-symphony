use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use symphony_core::domain::Workspace;
use symphony_core::error::SymphonyError;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, warn};

use crate::hooks::run_hook;
use crate::safety::validate_workspace_path;
use crate::sanitizer::sanitize_workspace_key;

/// Manages on-disk workspace directories for issue processing.
///
/// Each issue gets a dedicated workspace directory under `root`. Lifecycle
/// hooks can be configured to run at creation time, before/after agent runs,
/// and before workspace removal.
pub struct WorkspaceManager {
    root: PathBuf,
    after_create_hook: Option<String>,
    before_run_hook: Option<String>,
    after_run_hook: Option<String>,
    before_remove_hook: Option<String>,
    hook_timeout_ms: u64,
    prepare_locks: Mutex<HashMap<String, Arc<AsyncMutex<()>>>>,
}

impl WorkspaceManager {
    /// Create a new workspace manager.
    ///
    /// # Arguments
    ///
    /// * `root` - Base directory under which all workspaces are created.
    /// * `after_create` - Shell script to run after a workspace is created.
    /// * `before_run` - Shell script to run before an agent session starts.
    /// * `after_run` - Shell script to run after an agent session completes.
    /// * `before_remove` - Shell script to run before workspace removal.
    /// * `hook_timeout_ms` - Maximum wall-clock time for any hook execution.
    pub fn new(
        root: PathBuf,
        after_create: Option<String>,
        before_run: Option<String>,
        after_run: Option<String>,
        before_remove: Option<String>,
        hook_timeout_ms: u64,
    ) -> Self {
        Self {
            root,
            after_create_hook: after_create,
            before_run_hook: before_run,
            after_run_hook: after_run,
            before_remove_hook: before_remove,
            hook_timeout_ms,
            prepare_locks: Mutex::new(HashMap::new()),
        }
    }

    /// Compute the on-disk path for a given issue identifier.
    ///
    /// The identifier is sanitized before being used as the directory name.
    pub fn workspace_path_for(&self, identifier: &str) -> PathBuf {
        let key = sanitize_workspace_key(identifier);
        self.root.join(&key)
    }

    /// Create or locate a workspace and run the `before_run` hook under a
    /// per-workspace async mutex so concurrent same-issue workers do not race
    /// through initial git setup.
    pub async fn prepare_for_issue(
        &self,
        identifier: &str,
        issue_id: Option<&str>,
        issue_identifier: Option<&str>,
    ) -> Result<Workspace, SymphonyError> {
        let workspace_key = sanitize_workspace_key(identifier);
        let lock = {
            let mut locks = self.prepare_locks.lock().unwrap();
            locks
                .entry(workspace_key)
                .or_insert_with(|| Arc::new(AsyncMutex::new(())))
                .clone()
        };

        let _guard = lock.lock().await;
        let workspace = self.create_for_issue(identifier).await?;
        self.run_before_run_hook(&workspace.path, issue_id, issue_identifier)
            .await?;
        Ok(workspace)
    }

    /// Create (or locate) a workspace directory for the given issue.
    ///
    /// 1. Sanitize the identifier to a filesystem-safe workspace key.
    /// 2. Compute the target path under the workspace root.
    /// 3. Validate the path is strictly contained within the root.
    /// 4. Create the directory if it does not exist.
    /// 5. If newly created and an `after_create` hook is configured, run it.
    ///    On hook failure the directory is removed and the error propagated.
    /// 6. Return a `Workspace` descriptor.
    pub async fn create_for_issue(&self, identifier: &str) -> Result<Workspace, SymphonyError> {
        let workspace_key = sanitize_workspace_key(identifier);
        let path = self.root.join(&workspace_key);

        // Ensure the root directory itself exists so canonicalize works.
        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|e| SymphonyError::WorkspaceError {
                detail: format!(
                    "cannot create workspace root '{}': {e}",
                    self.root.display()
                ),
            })?;

        let already_exists = path.exists();
        tokio::fs::create_dir_all(&path)
            .await
            .map_err(|e| SymphonyError::WorkspaceError {
                detail: format!(
                    "cannot create workspace directory '{}': {e}",
                    path.display()
                ),
            })?;

        let created_now = !already_exists;

        // Validate containment after the directory exists (canonicalize needs it).
        validate_workspace_path(&path, &self.root)?;

        info!(
            workspace_key = %workspace_key,
            path = %path.display(),
            created_now,
            "workspace ready"
        );

        // Run after-create hook for newly created workspaces.
        if created_now {
            if let Some(ref script) = self.after_create_hook {
                if let Err(e) = run_hook(
                    "after_create",
                    script,
                    &path,
                    self.hook_timeout_ms,
                    Some(identifier),
                    Some(identifier),
                )
                .await
                {
                    warn!(
                        workspace_key = %workspace_key,
                        error = %e,
                        "after_create hook failed, removing workspace"
                    );
                    let _ = tokio::fs::remove_dir_all(&path).await;
                    return Err(e);
                }
            }
        }

        Ok(Workspace {
            path,
            workspace_key,
            created_now,
        })
    }

    /// Run the `before_run` hook if configured.
    ///
    /// Call this before starting an agent session in the workspace.
    /// Issue context is passed as environment variables to the hook script.
    pub async fn run_before_run_hook(
        &self,
        workspace_path: &Path,
        issue_id: Option<&str>,
        issue_identifier: Option<&str>,
    ) -> Result<(), SymphonyError> {
        if let Some(ref script) = self.before_run_hook {
            run_hook(
                "before_run",
                script,
                workspace_path,
                self.hook_timeout_ms,
                issue_id,
                issue_identifier,
            )
            .await?;
        }
        Ok(())
    }

    /// Run the `after_run` hook if configured (best effort).
    ///
    /// Call this after an agent session completes. Failures are logged but
    /// not propagated.
    pub async fn run_after_run_hook(
        &self,
        workspace_path: &Path,
        issue_id: Option<&str>,
        issue_identifier: Option<&str>,
    ) {
        if let Some(ref script) = self.after_run_hook {
            if let Err(e) = run_hook(
                "after_run",
                script,
                workspace_path,
                self.hook_timeout_ms,
                issue_id,
                issue_identifier,
            )
            .await
            {
                warn!(
                    path = %workspace_path.display(),
                    error = %e,
                    "after_run hook failed (non-fatal)"
                );
            }
        }
    }

    /// Remove a workspace directory for the given identifier.
    ///
    /// If a `before_remove` hook is configured it runs first (best effort).
    /// The directory is then removed recursively.
    pub async fn cleanup_workspace(&self, identifier: &str) -> Result<(), SymphonyError> {
        let workspace_key = sanitize_workspace_key(identifier);
        let path = self.root.join(&workspace_key);

        if let Some(ref script) = self.before_remove_hook {
            if let Err(e) = run_hook(
                "before_remove",
                script,
                &path,
                self.hook_timeout_ms,
                Some(identifier),
                Some(identifier),
            )
            .await
            {
                warn!(
                    workspace_key = %workspace_key,
                    error = %e,
                    "before_remove hook failed (best effort)"
                );
            }
        }

        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|e| SymphonyError::WorkspaceError {
                detail: format!("failed to remove workspace '{}': {e}", path.display()),
            })?;

        info!(workspace_key = %workspace_key, "workspace cleaned up");

        let mut locks = self.prepare_locks.lock().unwrap();
        locks.remove(&workspace_key);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_manager(root: PathBuf) -> WorkspaceManager {
        WorkspaceManager::new(root, None, None, None, None, 5000)
    }

    #[tokio::test]
    async fn create_for_issue_creates_directory() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_manager(tmp.path().to_path_buf());

        let ws = mgr.create_for_issue("#42").await.unwrap();
        assert_eq!(ws.workspace_key, "_42");
        assert!(ws.created_now);
        assert!(ws.path.exists());
        assert!(ws.path.is_dir());
    }

    #[tokio::test]
    async fn create_for_issue_existing_directory_not_created_now() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_manager(tmp.path().to_path_buf());

        let ws1 = mgr.create_for_issue("issue-1").await.unwrap();
        assert!(ws1.created_now);

        let ws2 = mgr.create_for_issue("issue-1").await.unwrap();
        assert!(!ws2.created_now);
        assert_eq!(ws1.path, ws2.path);
    }

    #[tokio::test]
    async fn workspace_path_for_returns_correct_path() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_manager(tmp.path().to_path_buf());
        let path = mgr.workspace_path_for("#99");
        assert_eq!(path, tmp.path().join("_99"));
    }

    #[tokio::test]
    async fn cleanup_removes_directory() {
        let tmp = TempDir::new().unwrap();
        let mgr = make_manager(tmp.path().to_path_buf());

        let ws = mgr.create_for_issue("to-remove").await.unwrap();
        assert!(ws.path.exists());

        mgr.cleanup_workspace("to-remove").await.unwrap();
        assert!(!ws.path.exists());
    }

    #[tokio::test]
    async fn after_create_hook_runs_on_new_workspace() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceManager::new(
            tmp.path().to_path_buf(),
            Some("touch hook_ran".to_owned()),
            None,
            None,
            None,
            5000,
        );

        let ws = mgr.create_for_issue("hooked").await.unwrap();
        assert!(ws.path.join("hook_ran").exists());
    }

    #[tokio::test]
    async fn after_create_hook_failure_removes_workspace() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceManager::new(
            tmp.path().to_path_buf(),
            Some("exit 1".to_owned()),
            None,
            None,
            None,
            5000,
        );

        let result = mgr.create_for_issue("will-fail").await;
        assert!(result.is_err());

        let expected_path = tmp.path().join("will-fail");
        assert!(!expected_path.exists());
    }

    #[tokio::test]
    async fn prepare_for_issue_serializes_before_run_for_same_workspace() {
        let tmp = TempDir::new().unwrap();
        let mgr = WorkspaceManager::new(
            tmp.path().to_path_buf(),
            None,
            Some(
                r#"
if [ -f .before_run_lock ]; then
  exit 23
fi
touch .before_run_lock
sleep 0.2
echo "$SYMPHONY_ISSUE_IDENTIFIER" >> before_run.log
rm .before_run_lock
"#
                .trim()
                .to_owned(),
            ),
            None,
            None,
            5_000,
        );

        let (left, right) = tokio::join!(
            mgr.prepare_for_issue("same-issue", Some("42"), Some("#42")),
            mgr.prepare_for_issue("same-issue", Some("42"), Some("#42"))
        );

        let left = left.unwrap();
        let right = right.unwrap();
        assert_eq!(left.path, right.path);

        let before_run_log = tokio::fs::read_to_string(left.path.join("before_run.log"))
            .await
            .unwrap();
        let lines: Vec<&str> = before_run_log.lines().collect();
        assert_eq!(lines, vec!["#42", "#42"]);
    }
}
