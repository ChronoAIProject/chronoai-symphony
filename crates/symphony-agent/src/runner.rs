//! Agent runner -- manages process lifecycle and turn execution.
//!
//! The `AgentRunner` launches agent processes, performs the handshake,
//! and provides a turn-by-turn execution interface. The orchestrator's
//! worker task calls into the runner in a loop, checking issue state
//! between turns.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::Value;
use symphony_core::domain::{Issue, ServiceConfig};
use symphony_core::error::SymphonyError;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::approval_handler::ApprovalHandler;
use crate::process::AgentProcess;
use crate::protocol::events::AgentEvent;
use crate::protocol::handshake::{perform_handshake, SessionInfo};
use crate::protocol::messages::{
    build_turn_start, default_approval_policy, default_thread_sandbox,
    default_turn_sandbox_policy,
};
use crate::protocol::streaming::{stream_turn, TurnResult};
use crate::timeout::TimeoutConfig;

/// A live agent session with an active subprocess and thread context.
pub struct AgentSession {
    pub process: AgentProcess,
    pub session_info: SessionInfo,
    pub workspace_path: PathBuf,
}

/// Exit reason for a worker run.
#[derive(Debug)]
pub enum WorkerExitReason {
    Normal,
    Failed(String),
}

/// Manages the agent lifecycle: process launch, handshake, and turn execution.
pub struct AgentRunner {
    config: ServiceConfig,
}

impl AgentRunner {
    pub fn new(config: ServiceConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &ServiceConfig {
        &self.config
    }

    /// Resolve the approval policy as a JSON Value.
    /// Uses config override if present, otherwise uses OpenAI's default.
    fn resolve_approval_policy(&self) -> Value {
        match &self.config.codex_approval_policy {
            Some(s) => {
                // Try parsing as JSON first (could be a map like {"reject": {...}}).
                serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone()))
            }
            None => default_approval_policy(),
        }
    }

    fn resolve_thread_sandbox(&self) -> Value {
        match &self.config.codex_thread_sandbox {
            Some(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
            None => default_thread_sandbox(),
        }
    }

    fn resolve_turn_sandbox_policy(&self, workspace_path: &str) -> Value {
        let mut policy = match &self.config.codex_turn_sandbox_policy {
            Some(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
            None => default_turn_sandbox_policy(workspace_path),
        };
        // Override networkAccess from config.
        if let Some(obj) = policy.as_object_mut() {
            obj.insert(
                "networkAccess".to_string(),
                Value::Bool(self.config.codex_network_access),
            );
        }
        policy
    }

    /// Build the agent command with optional model and reasoning effort flags.
    fn build_command(&self, base_command: &str) -> String {
        let mut cmd = base_command.to_string();
        if let Some(ref model) = self.config.codex_model {
            cmd = format!("{cmd} --model {model}");
        }
        if let Some(ref effort) = self.config.codex_reasoning_effort {
            cmd = format!("{cmd} --config model_reasoning_effort={effort}");
        }
        cmd
    }

    /// Start a new agent session: launch process and perform handshake.
    pub async fn start_session(
        &self,
        workspace_path: &Path,
        issue: &Issue,
        prompt: &str,
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<AgentSession, SymphonyError> {
        // Build the command with optional model and reasoning effort flags.
        let base_command = &self.config.codex_command;
        let command = self.build_command(base_command);
        let cwd = workspace_path.to_string_lossy().to_string();

        let ap = self.resolve_approval_policy();
        let sb = self.resolve_thread_sandbox();
        let sp = self.resolve_turn_sandbox_policy(&cwd);

        let title = format!("{}: {}", issue.identifier, issue.title);

        info!(
            issue_id = %issue.id,
            command = %command,
            cwd = %cwd,
            "starting agent session"
        );

        let mut process = match AgentProcess::launch(&command, workspace_path).await {
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
            &title,
            Some(&ap),
            Some(&sb),
            Some(&sp),
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
        approval_handler: &dyn ApprovalHandler,
    ) -> Result<TurnResult, SymphonyError> {
        let timeout_config = self.build_timeout_config();

        if !is_first_turn {
            let cwd = session.workspace_path.to_string_lossy().to_string();
            let ap = self.resolve_approval_policy();
            let sp = self.resolve_turn_sandbox_policy(&cwd);
            let title = format!("{}: {}", issue.identifier, issue.title);

            let turn_req = build_turn_start(
                session.process.pid().unwrap_or(0) as u64,
                &session.session_info.thread_id,
                prompt,
                &cwd,
                &title,
                &ap,
                &sp,
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
            approval_handler,
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

    fn build_timeout_config(&self) -> TimeoutConfig {
        TimeoutConfig::new(
            self.config.codex_read_timeout_ms,
            self.config.codex_turn_timeout_ms,
            self.config.codex_stall_timeout_ms,
        )
    }
}
