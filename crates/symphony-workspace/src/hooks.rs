use std::path::Path;

use symphony_core::error::SymphonyError;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tracing::{info, warn};

/// Maximum number of characters to capture from stdout/stderr for logging.
const MAX_OUTPUT_CHARS: usize = 1000;

/// Execute a shell hook script with a timeout.
///
/// The script is run via `bash -lc <script>` with `cwd` as the working
/// directory. If the command does not complete within `timeout_ms`
/// milliseconds, the child process is killed and a `HookTimeout` error is
/// returned. A non-zero exit code produces a `HookError`.
pub async fn run_hook(
    hook_name: &str,
    script: &str,
    cwd: &Path,
    timeout_ms: u64,
    issue_id: Option<&str>,
    issue_identifier: Option<&str>,
) -> Result<(), SymphonyError> {
    info!(hook = hook_name, cwd = %cwd.display(), "starting hook");

    let mut cmd = Command::new("bash");
    cmd.arg("-lc")
        .arg(script)
        .current_dir(cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Pass issue context as environment variables so hooks can use them
    // for branch naming, logging, etc.
    if let Some(id) = issue_id {
        cmd.env("SYMPHONY_ISSUE_ID", id);
    }
    if let Some(ident) = issue_identifier {
        cmd.env("SYMPHONY_ISSUE_IDENTIFIER", ident);
        // Also provide just the number (e.g., "68" from "#68").
        let num = ident.trim_start_matches('#');
        cmd.env("SYMPHONY_ISSUE_NUMBER", num);
    }

    let mut child = cmd.spawn()
        .map_err(|e| SymphonyError::HookError {
            hook: hook_name.to_owned(),
            detail: format!("failed to spawn bash: {e}"),
        })?;

    // Take ownership of the stdio handles so we can read them independently
    // of the child lifetime, allowing us to still call kill() on timeout.
    let mut stdout_handle = child.stdout.take();
    let mut stderr_handle = child.stderr.take();

    let timeout_duration = std::time::Duration::from_millis(timeout_ms);

    let wait_fut = async {
        let status = child.wait().await?;

        let mut stdout_buf = Vec::new();
        if let Some(ref mut handle) = stdout_handle {
            let _ = handle.read_to_end(&mut stdout_buf).await;
        }

        let mut stderr_buf = Vec::new();
        if let Some(ref mut handle) = stderr_handle {
            let _ = handle.read_to_end(&mut stderr_buf).await;
        }

        Ok::<_, std::io::Error>((status, stdout_buf, stderr_buf))
    };

    let result = tokio::time::timeout(timeout_duration, wait_fut).await;

    match result {
        Ok(Ok((status, stdout_buf, stderr_buf))) => {
            let stdout = truncate_output(&String::from_utf8_lossy(&stdout_buf));
            let stderr = truncate_output(&String::from_utf8_lossy(&stderr_buf));

            if !stdout.is_empty() {
                info!(hook = hook_name, stdout = %stdout, "hook stdout");
            }
            if !stderr.is_empty() {
                warn!(hook = hook_name, stderr = %stderr, "hook stderr");
            }

            if status.success() {
                info!(hook = hook_name, "hook completed successfully");
                Ok(())
            } else {
                let code = status.code();
                warn!(hook = hook_name, exit_code = ?code, "hook failed");
                Err(SymphonyError::HookError {
                    hook: hook_name.to_owned(),
                    detail: format!(
                        "exited with code {code:?}; stderr: {stderr}"
                    ),
                })
            }
        }
        Ok(Err(e)) => {
            warn!(hook = hook_name, error = %e, "hook I/O error");
            Err(SymphonyError::HookError {
                hook: hook_name.to_owned(),
                detail: format!("I/O error waiting for process: {e}"),
            })
        }
        Err(_elapsed) => {
            warn!(hook = hook_name, timeout_ms, "hook timed out, killing process");
            // Best-effort kill; ignore errors if the process already exited.
            let _ = child.kill().await;
            Err(SymphonyError::HookTimeout {
                hook: hook_name.to_owned(),
                timeout_ms,
            })
        }
    }
}

/// Truncate output to at most `MAX_OUTPUT_CHARS` characters.
fn truncate_output(s: &str) -> String {
    let trimmed = s.trim();
    if trimmed.len() <= MAX_OUTPUT_CHARS {
        trimmed.to_owned()
    } else {
        format!("{}...(truncated)", &trimmed[..MAX_OUTPUT_CHARS])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn runs_simple_echo() {
        let dir = TempDir::new().unwrap();
        let result = run_hook("test-echo", "echo hello", dir.path(), 5000, None, None).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn non_zero_exit_returns_hook_error() {
        let dir = TempDir::new().unwrap();
        let result = run_hook("test-fail", "exit 1", dir.path(), 5000, None, None).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SymphonyError::HookError { hook, .. } => {
                assert_eq!(hook, "test-fail");
            }
            other => panic!("expected HookError, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_returns_hook_timeout() {
        let dir = TempDir::new().unwrap();
        let result = run_hook("test-timeout", "sleep 60", dir.path(), 100, None, None).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            SymphonyError::HookTimeout { hook, timeout_ms } => {
                assert_eq!(hook, "test-timeout");
                assert_eq!(timeout_ms, 100);
            }
            other => panic!("expected HookTimeout, got {other:?}"),
        }
    }

    #[test]
    fn truncate_output_short_string() {
        let short = "hello world";
        assert_eq!(truncate_output(short), "hello world");
    }

    #[test]
    fn truncate_output_long_string() {
        let long = "a".repeat(2000);
        let result = truncate_output(&long);
        assert!(result.contains("...(truncated)"));
        assert!(result.len() < 2000);
    }
}
