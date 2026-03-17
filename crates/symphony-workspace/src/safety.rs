use std::path::Path;

use regex::Regex;
use symphony_core::error::SymphonyError;

/// Validate that `workspace_path` is strictly contained within `workspace_root`.
///
/// Both paths are canonicalized to resolve symlinks, `..` components, and
/// relative segments before comparison. The workspace directory name (final
/// component) must also be a valid sanitized identifier containing only
/// `[A-Za-z0-9._-]`.
pub fn validate_workspace_path(
    workspace_path: &Path,
    workspace_root: &Path,
) -> Result<(), SymphonyError> {
    let canonical_root = workspace_root.canonicalize().map_err(|e| {
        SymphonyError::WorkspaceError {
            detail: format!(
                "cannot canonicalize workspace root '{}': {e}",
                workspace_root.display()
            ),
        }
    })?;

    let canonical_path = workspace_path.canonicalize().map_err(|e| {
        SymphonyError::WorkspaceError {
            detail: format!(
                "cannot canonicalize workspace path '{}': {e}",
                workspace_path.display()
            ),
        }
    })?;

    // The workspace path must be a child of the root, not equal to it.
    if !canonical_path.starts_with(&canonical_root) || canonical_path == canonical_root {
        return Err(SymphonyError::WorkspaceError {
            detail: format!(
                "workspace path '{}' is not inside workspace root '{}'",
                canonical_path.display(),
                canonical_root.display()
            ),
        });
    }

    // Validate the workspace key (final path component) contains only safe chars.
    if let Some(key) = canonical_path
        .file_name()
        .and_then(|name| name.to_str())
    {
        let valid_key = Regex::new(r"^[A-Za-z0-9._\-]+$").expect("invalid regex");
        if !valid_key.is_match(key) {
            return Err(SymphonyError::WorkspaceError {
                detail: format!(
                    "workspace key '{key}' contains invalid characters; \
                     only [A-Za-z0-9._-] are allowed"
                ),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn valid_child_directory() {
        let root = TempDir::new().unwrap();
        let child = root.path().join("my-workspace");
        std::fs::create_dir(&child).unwrap();

        assert!(validate_workspace_path(&child, root.path()).is_ok());
    }

    #[test]
    fn rejects_path_outside_root() {
        let root = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let outside_child = outside.path().join("sneaky");
        std::fs::create_dir(&outside_child).unwrap();

        let result = validate_workspace_path(&outside_child, root.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not inside workspace root"));
    }

    #[test]
    fn rejects_root_itself() {
        let root = TempDir::new().unwrap();
        let result = validate_workspace_path(root.path(), root.path());
        assert!(result.is_err());
    }

    #[test]
    fn rejects_invalid_key_characters() {
        let root = TempDir::new().unwrap();
        // Create a directory with a space in the name (invalid key).
        let bad_dir = root.path().join("bad name");
        std::fs::create_dir(&bad_dir).unwrap();

        let result = validate_workspace_path(&bad_dir, root.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("invalid characters"));
    }

    #[test]
    fn nonexistent_path_returns_error() {
        let root = TempDir::new().unwrap();
        let nonexistent = root.path().join("does-not-exist");

        let result = validate_workspace_path(&nonexistent, root.path());
        assert!(result.is_err());
    }
}
