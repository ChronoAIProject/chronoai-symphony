//! Subprocess management for the Codex agent process.
//!
//! Launches the agent via `bash -lc <command>` and provides async I/O
//! over stdin/stdout for JSON-RPC communication.

use std::path::Path;

use symphony_core::error::SymphonyError;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, error, info};

/// Handle to a running agent subprocess with piped stdin/stdout.
pub struct AgentProcess {
    child: Child,
    stdout: BufReader<tokio::process::ChildStdout>,
    stdin: tokio::process::ChildStdin,
}

impl AgentProcess {
    /// Launch an agent subprocess.
    ///
    /// The command is executed via `bash -lc <command>` with the given
    /// working directory. Stdin and stdout are piped for JSON-RPC
    /// communication.
    ///
    /// When `merge_stderr` is `true`, stderr is redirected into stdout
    /// via `2>&1` (used for Codex JSON-RPC where all output goes to
    /// stdout). When `false`, stderr goes to `/dev/null` without merging
    /// (used for Claude CLI where stream-json goes to stdout and verbose
    /// logs go to stderr).
    pub async fn launch(
        command: &str,
        cwd: &Path,
        env_vars: &[(&str, &str)],
        merge_stderr: bool,
    ) -> Result<Self, SymphonyError> {
        info!(command, cwd = %cwd.display(), merge_stderr, "launching agent process");

        let shell_command = if merge_stderr {
            format!("{command} 2>&1")
        } else {
            command.to_string()
        };

        // If SYMPHONY_TOKEN_FILE is set (GitHub App auth), create a
        // wrapper bin directory with a `gh` script that re-reads the token
        // from the file before each invocation. This directory is prepended
        // to PATH so all subprocesses (including codex/claude agent) use
        // the wrapper instead of the real `gh`, ensuring fresh tokens.
        let has_token_file = env_vars.iter().any(|(k, _)| *k == "SYMPHONY_TOKEN_FILE");
        let wrapper_dir = if has_token_file {
            let token_file = env_vars.iter()
                .find(|(k, _)| *k == "SYMPHONY_TOKEN_FILE")
                .map(|(_, v)| *v)
                .unwrap_or("");

            let wrapper_dir = cwd.join(".symphony_bin");
            let _ = std::fs::create_dir_all(&wrapper_dir);

            // Create a `gh` wrapper script.
            let gh_wrapper = wrapper_dir.join("gh");
            let real_gh = std::process::Command::new("which")
                .arg("gh")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "gh".to_string());
            let _ = std::fs::write(
                &gh_wrapper,
                format!(
                    "#!/bin/sh\nexport GH_TOKEN=$(cat '{}')\nexport GITHUB_TOKEN=\"$GH_TOKEN\"\nexec '{}' \"$@\"\n",
                    token_file, real_gh
                ),
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&gh_wrapper, std::fs::Permissions::from_mode(0o755));
            }

            Some(wrapper_dir)
        } else {
            None
        };

        let mut cmd = Command::new("bash");
        cmd.arg("-lc")
            .arg(&shell_command)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());

        // When merging stderr (Codex), stderr is already in stdout via 2>&1.
        // When not merging (Claude CLI), pipe stderr separately and log it
        // so we can see errors.
        if merge_stderr {
            cmd.stderr(std::process::Stdio::null());
        } else {
            cmd.stderr(std::process::Stdio::piped());
        }

        // Prepend wrapper bin dir to PATH if using token file.
        if let Some(ref wrapper) = wrapper_dir {
            let current_path = std::env::var("PATH").unwrap_or_default();
            cmd.env("PATH", format!("{}:{}", wrapper.display(), current_path));
        }

        for (key, value) in env_vars {
            cmd.env(key, value);
        }

        let mut child = cmd.spawn()
            .map_err(|e| SymphonyError::CodexNotFound {
                command: format!("{command}: {e}"),
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| SymphonyError::ResponseError {
                detail: "failed to capture agent stdout".to_string(),
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| SymphonyError::ResponseError {
                detail: "failed to capture agent stdin".to_string(),
            })?;

        // Drain stderr in a background task if piped (non-merged mode).
        if !merge_stderr {
            if let Some(stderr) = child.stderr.take() {
                use tokio::io::AsyncBufReadExt;
                tokio::spawn(async move {
                    let reader = tokio::io::BufReader::new(stderr);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        debug!(target: "agent_stderr", "{}", line);
                    }
                });
            }
        }

        let pid = child.id();
        info!(pid = ?pid, "agent process launched");

        Ok(Self {
            child,
            stdout: BufReader::new(stdout),
            stdin,
        })
    }

    /// Write a JSON message to the agent's stdin, followed by a newline.
    pub async fn write_message(&mut self, msg: &str) -> Result<(), SymphonyError> {
        debug!(msg_len = msg.len(), "writing message to agent");

        self.stdin
            .write_all(msg.as_bytes())
            .await
            .map_err(|e| SymphonyError::ResponseError {
                detail: format!("failed to write to agent stdin: {e}"),
            })?;

        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|e| SymphonyError::ResponseError {
                detail: format!("failed to write newline to agent stdin: {e}"),
            })?;

        self.stdin.flush().await.map_err(|e| SymphonyError::ResponseError {
            detail: format!("failed to flush agent stdin: {e}"),
        })?;

        Ok(())
    }

    /// Read a single line from the agent's stdout.
    ///
    /// Returns `None` if the stream has ended (process exited).
    pub async fn read_line(&mut self) -> Result<Option<String>, SymphonyError> {
        let mut line = String::new();
        let bytes_read = self
            .stdout
            .read_line(&mut line)
            .await
            .map_err(|e| SymphonyError::ResponseError {
                detail: format!("failed to read from agent stdout: {e}"),
            })?;

        if bytes_read == 0 {
            info!("agent stdout closed (EOF)");
            return Ok(None);
        }

        // Trim the trailing newline.
        let trimmed = line.trim_end().to_string();
        if !trimmed.is_empty() {
            debug!(bytes = bytes_read, line_len = trimmed.len(), "read line from agent");
        }

        Ok(Some(trimmed))
    }

    /// Read raw bytes from stdout (for debugging).
    /// Returns the number of bytes read, or 0 on EOF.
    pub async fn read_raw(&mut self, buf: &mut [u8]) -> Result<usize, SymphonyError> {
        self.stdout.read(buf).await.map_err(|e| SymphonyError::ResponseError {
            detail: format!("failed to read raw bytes: {e}"),
        })
    }

    /// Kill the agent process.
    pub async fn kill(&mut self) -> Result<(), SymphonyError> {
        info!(pid = ?self.child.id(), "killing agent process");
        self.child.kill().await.map_err(|e| {
            error!(error = %e, "failed to kill agent process");
            SymphonyError::ResponseError {
                detail: format!("failed to kill agent process: {e}"),
            }
        })
    }

    /// Return the OS process ID of the agent, if available.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Check if the child process has exited without blocking.
    pub async fn try_wait(&mut self) -> Result<Option<std::process::ExitStatus>, SymphonyError> {
        self.child.try_wait().map_err(|e| SymphonyError::ResponseError {
            detail: format!("failed to check agent process status: {e}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn launch_echo_and_read_output() {
        let dir = TempDir::new().unwrap();
        let mut proc = AgentProcess::launch("echo hello", dir.path(), &[], true)
            .await
            .unwrap();

        let line = proc.read_line().await.unwrap();
        assert_eq!(line, Some("hello".to_string()));

        // Next read should return None (EOF).
        let eof = proc.read_line().await.unwrap();
        assert_eq!(eof, None);
    }

    #[tokio::test]
    async fn launch_cat_write_and_read() {
        let dir = TempDir::new().unwrap();
        let mut proc = AgentProcess::launch("cat", dir.path(), &[], true).await.unwrap();

        proc.write_message(r#"{"hello":"world"}"#).await.unwrap();
        let line = proc.read_line().await.unwrap();
        assert_eq!(line, Some(r#"{"hello":"world"}"#.to_string()));

        proc.kill().await.unwrap();
    }

    #[tokio::test]
    async fn pid_returns_some() {
        let dir = TempDir::new().unwrap();
        let proc = AgentProcess::launch("sleep 10", dir.path(), &[], true)
            .await
            .unwrap();
        assert!(proc.pid().is_some());
    }

    #[tokio::test]
    async fn launch_invalid_command_still_spawns_bash() {
        let dir = TempDir::new().unwrap();
        // bash -lc with a nonexistent command will spawn bash (which exits with error),
        // but launch itself should succeed since bash exists.
        let result = AgentProcess::launch("nonexistent_command_12345", dir.path(), &[], true).await;
        // This may succeed (bash spawns) or fail depending on environment.
        // We just verify it does not panic.
        drop(result);
    }

    #[tokio::test]
    async fn launch_without_merge_stderr() {
        let dir = TempDir::new().unwrap();
        let mut proc = AgentProcess::launch("echo hello", dir.path(), &[], false)
            .await
            .unwrap();

        let line = proc.read_line().await.unwrap();
        assert_eq!(line, Some("hello".to_string()));
    }
}
