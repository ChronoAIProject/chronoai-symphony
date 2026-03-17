//! Subprocess management for the Codex agent process.
//!
//! Launches the agent via `bash -lc <command>` and provides async I/O
//! over stdin/stdout for JSON-RPC communication.

use std::path::Path;

use symphony_core::error::SymphonyError;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, error, info, warn};

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
    /// communication; stderr is piped and logged separately.
    pub async fn launch(command: &str, cwd: &Path) -> Result<Self, SymphonyError> {
        info!(command, cwd = %cwd.display(), "launching agent process");

        let mut child = Command::new("bash")
            .arg("-lc")
            .arg(command)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
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

        let stderr = child.stderr.take();

        // Spawn a background task to drain and log stderr.
        if let Some(stderr) = stderr {
            tokio::spawn(async move {
                let reader = BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    warn!(target: "agent_stderr", "{}", line);
                }
            });
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
            debug!("agent stdout closed (EOF)");
            return Ok(None);
        }

        // Trim the trailing newline.
        let trimmed = line.trim_end().to_string();
        if !trimmed.is_empty() {
            debug!(line_len = trimmed.len(), "read line from agent");
        }

        Ok(Some(trimmed))
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
        let mut proc = AgentProcess::launch("echo hello", dir.path())
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
        let mut proc = AgentProcess::launch("cat", dir.path()).await.unwrap();

        proc.write_message(r#"{"hello":"world"}"#).await.unwrap();
        let line = proc.read_line().await.unwrap();
        assert_eq!(line, Some(r#"{"hello":"world"}"#.to_string()));

        proc.kill().await.unwrap();
    }

    #[tokio::test]
    async fn pid_returns_some() {
        let dir = TempDir::new().unwrap();
        let proc = AgentProcess::launch("sleep 10", dir.path())
            .await
            .unwrap();
        assert!(proc.pid().is_some());
    }

    #[tokio::test]
    async fn launch_invalid_command_still_spawns_bash() {
        let dir = TempDir::new().unwrap();
        // bash -lc with a nonexistent command will spawn bash (which exits with error),
        // but launch itself should succeed since bash exists.
        let result = AgentProcess::launch("nonexistent_command_12345", dir.path()).await;
        // This may succeed (bash spawns) or fail depending on environment.
        // We just verify it does not panic.
        drop(result);
    }
}
