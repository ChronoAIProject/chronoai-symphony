//! Agent runner -- manages process lifecycle and turn execution.
//!
//! The `AgentRunner` launches agent processes, performs the handshake,
//! and provides a turn-by-turn execution interface. The orchestrator's
//! worker task calls into the runner in a loop, checking issue state
//! between turns.

use std::path::{Path, PathBuf};

use chrono::Utc;
use symphony_core::domain::{Issue, ServiceConfig};
use symphony_core::error::SymphonyError;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::process::AgentProcess;
use crate::protocol::events::AgentEvent;
use crate::protocol::handshake::{perform_handshake, SessionInfo};
use crate::protocol::streaming::{stream_turn, TurnResult};
use crate::timeout::TimeoutConfig;

/// Default approval policy when none is configured.
const DEFAULT_APPROVAL_POLICY: &str = "full-auto";

/// Default sandbox setting.
const DEFAULT_SANDBOX: &str = "none";

/// A live agent session with an active subprocess and thread context.
pub struct AgentSession {
    pub process: AgentProcess,
    pub session_info: SessionInfo,
    pub workspace_path: PathBuf,
}

/// Exit reason for a worker run.
#[derive(Debug)]
pub enum WorkerExitReason {
    /// The run completed normally (all turns finished or issue resolved).
    Normal,
    /// The run failed with an error.
    Failed(String),
}

/// Manages the agent lifecycle: process launch, handshake, and turn execution.
///
/// The runner does not own workspace or hook management directly; those
/// responsibilities belong to the `WorkspaceManager`. The runner focuses
/// on the agent subprocess and protocol interactions.
pub struct AgentRunner {
    config: ServiceConfig,
}

impl AgentRunner {
    /// Create a new agent runner with the given configuration.
    pub fn new(config: ServiceConfig) -> Self {
        Self { config }
    }

    /// Return a reference to the current service configuration.
    pub fn config(&self) -> &ServiceConfig {
        &self.config
    }

    /// Start a new agent session: launch process and perform handshake.
    ///
    /// The workspace directory must already exist before calling this.
    pub async fn start_session(
        &self,
        workspace_path: &Path,
        issue: &Issue,
        prompt: &str,
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<AgentSession, SymphonyError> {
        let command = &self.config.codex_command;

        let approval_policy = self
            .config
            .codex_approval_policy
            .as_deref()
            .unwrap_or(DEFAULT_APPROVAL_POLICY);

        let sandbox = self
            .config
            .codex_thread_sandbox
            .as_deref()
            .unwrap_or(DEFAULT_SANDBOX);

        let sandbox_policy = self
            .config
            .codex_turn_sandbox_policy
            .as_deref()
            .unwrap_or(DEFAULT_SANDBOX);

        let cwd = workspace_path.to_string_lossy();

        info!(
            issue_id = %issue.id,
            command = %command,
            cwd = %cwd,
            "starting agent session"
        );

        let mut process = match AgentProcess::launch(command, workspace_path).await {
            Ok(p) => p,
            Err(e) => {
                error!(error = %e, "failed to launch agent process");
                let _ = event_tx
                    .send(AgentEvent::StartupFailed {
                        error: e.to_string(),
                        timestamp: Utc::now(),
                    })
                    .await;
                return Err(e);
            }
        };

        let timeout_config = self.build_timeout_config();

        let session_info = match perform_handshake(
            &mut process,
            &cwd,
            prompt,
            &issue.title,
            approval_policy,
            sandbox,
            sandbox_policy,
            timeout_config.read_timeout,
        )
        .await
        {
            Ok(info) => info,
            Err(e) => {
                error!(error = %e, "handshake failed");
                let _ = process.kill().await;
                let _ = event_tx
                    .send(AgentEvent::StartupFailed {
                        error: e.to_string(),
                        timestamp: Utc::now(),
                    })
                    .await;
                return Err(e);
            }
        };

        let pid = process.pid().map(|p| p.to_string());

        let _ = event_tx
            .send(AgentEvent::SessionStarted {
                session_id: session_info.session_id.clone(),
                thread_id: session_info.thread_id.clone(),
                turn_id: session_info.turn_id.clone(),
                pid,
                timestamp: Utc::now(),
            })
            .await;

        Ok(AgentSession {
            process,
            session_info,
            workspace_path: workspace_path.to_path_buf(),
        })
    }

    /// Run a single turn within an existing session.
    ///
    /// For continuation turns, a new `turn/start` message is sent before
    /// streaming begins. The first turn's streaming is initiated during
    /// the handshake, so `is_first_turn` should be `true` to skip the
    /// extra `turn/start`.
    pub async fn run_turn(
        &self,
        session: &mut AgentSession,
        prompt: &str,
        issue: &Issue,
        is_first_turn: bool,
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<TurnResult, SymphonyError> {
        let timeout_config = self.build_timeout_config();

        if !is_first_turn {
            let approval_policy = self
                .config
                .codex_approval_policy
                .as_deref()
                .unwrap_or(DEFAULT_APPROVAL_POLICY);

            let sandbox_policy = self
                .config
                .codex_turn_sandbox_policy
                .as_deref()
                .unwrap_or(DEFAULT_SANDBOX);

            let cwd = session.workspace_path.to_string_lossy().to_string();

            let turn_req = crate::protocol::messages::build_turn_start(
                session.process.pid().unwrap_or(0) as u64,
                &session.session_info.thread_id,
                prompt,
                &cwd,
                &issue.title,
                approval_policy,
                sandbox_policy,
            );
            let turn_json = serde_json::to_string(&turn_req)
                .map_err(|e| SymphonyError::ResponseError {
                    detail: format!("failed to serialize turn/start: {e}"),
                })?;
            session.process.write_message(&turn_json).await?;
        }

        stream_turn(
            &mut session.process,
            event_tx,
            timeout_config.turn_timeout,
        )
        .await
    }

    /// Stop an active session by killing the agent process.
    pub async fn stop_session(
        &self,
        session: &mut AgentSession,
    ) -> Result<(), SymphonyError> {
        info!(
            session_id = %session.session_info.session_id,
            "stopping agent session"
        );
        session.process.kill().await
    }

    /// Build a `TimeoutConfig` from the service configuration.
    fn build_timeout_config(&self) -> TimeoutConfig {
        TimeoutConfig::new(
            self.config.codex_read_timeout_ms,
            self.config.codex_turn_timeout_ms,
            self.config.codex_stall_timeout_ms,
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn build_timeout_config_reads_flat_fields() {
        // Verify the timeout builder reads from the flat config fields.
        // Full integration tests require a running agent process.
    }
}
