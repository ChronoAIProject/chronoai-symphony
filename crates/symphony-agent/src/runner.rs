//! Agent runner -- manages process lifecycle and turn execution.
//!
//! The `AgentRunner` launches agent processes, performs the handshake,
//! and provides a turn-by-turn execution interface. The orchestrator's
//! worker task calls into the runner in a loop, checking issue state
//! between turns.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::Value;
use symphony_core::domain::{AgentProfileConfig, Issue};
use symphony_core::error::SymphonyError;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::approval_handler::ApprovalHandler;
use crate::process::AgentProcess;
use crate::protocol::claude_cli;
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
///
/// Each `AgentRunner` is configured with a single `AgentProfileConfig`
/// describing the specific agent backend to use (command, model, timeouts,
/// sandbox policy, etc.). The orchestrator creates one runner per issue,
/// selecting the appropriate profile based on issue labels.
pub struct AgentRunner {
    profile: AgentProfileConfig,
}

impl AgentRunner {
    /// Create a runner from an `AgentProfileConfig`.
    pub fn new(profile: AgentProfileConfig) -> Self {
        Self { profile }
    }

    /// Return a reference to the underlying profile.
    pub fn profile(&self) -> &AgentProfileConfig {
        &self.profile
    }

    /// Resolve the approval policy as a JSON Value.
    /// Uses config override if present, otherwise uses OpenAI's default.
    fn resolve_approval_policy(&self) -> Value {
        match &self.profile.approval_policy {
            Some(s) => {
                // Try parsing as JSON first (could be a map like {"reject": {...}}).
                serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone()))
            }
            None => default_approval_policy(),
        }
    }

    fn resolve_thread_sandbox(&self) -> Value {
        match &self.profile.thread_sandbox {
            Some(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
            None => default_thread_sandbox(),
        }
    }

    fn resolve_turn_sandbox_policy(&self, workspace_path: &str) -> Value {
        let mut policy = match &self.profile.turn_sandbox_policy {
            Some(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
            None => default_turn_sandbox_policy(workspace_path),
        };
        // Override networkAccess from config.
        if let Some(obj) = policy.as_object_mut() {
            obj.insert(
                "networkAccess".to_string(),
                Value::Bool(self.profile.network_access),
            );
        }
        policy
    }

    /// Build the Codex app-server command with optional config flags.
    ///
    /// Codex uses `--config key=value` for all settings (not `--model`).
    fn build_command(&self, base_command: &str) -> String {
        let mut cmd = base_command.to_string();
        if let Some(ref model) = self.profile.model {
            cmd = format!("{cmd} -c model={model}");
        }
        if let Some(ref effort) = self.profile.reasoning_effort {
            cmd = format!("{cmd} -c model_reasoning_effort={effort}");
        }
        cmd
    }

    /// Collect environment variables to pass to the agent subprocess.
    /// These are set per-process (not global) so parallel agents with
    /// different configs don't conflict.
    ///
    /// If a `SYMPHONY_TOKEN_FILE` env var is set (GitHub App auth), reads
    /// the latest token from the file and sets `GH_TOKEN` / `GITHUB_TOKEN`
    /// on the subprocess. This ensures each new session gets a fresh token.
    fn build_env_vars(&self) -> Vec<(String, String)> {
        let mut vars = Vec::new();
        if let Some(ref model) = self.profile.model {
            vars.push(("SYMPHONY_AGENT_MODEL".to_string(), model.clone()));
        }
        if let Some(ref effort) = self.profile.reasoning_effort {
            vars.push(("MODEL_REASONING_EFFORT".to_string(), effort.clone()));
            vars.push(("SYMPHONY_REASONING_EFFORT".to_string(), effort.clone()));
        }

        // If using GitHub App auth, pass the token file path so the
        // agent can re-read fresh tokens. We also pass a fresh token
        // for the initial launch, but for long-running sessions the
        // agent should use the token file.
        if let Ok(token_file) = std::env::var("SYMPHONY_TOKEN_FILE") {
            vars.push(("SYMPHONY_TOKEN_FILE".to_string(), token_file.clone()));
            if let Ok(fresh_token) = std::fs::read_to_string(&token_file) {
                let token = fresh_token.trim().to_string();
                if !token.is_empty() {
                    vars.push(("GH_TOKEN".to_string(), token.clone()));
                    vars.push(("GITHUB_TOKEN".to_string(), token));
                }
            }
        }

        vars
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
        let base_command = &self.profile.command;
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

        let env_vars = self.build_env_vars();
        let env_refs: Vec<(&str, &str)> = env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let mut process = match AgentProcess::launch(&command, workspace_path, &env_refs, true).await {
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
            self.profile.read_timeout_ms,
            self.profile.turn_timeout_ms,
            self.profile.stall_timeout_ms,
        )
    }

    /// Build the Claude CLI command string.
    ///
    /// Constructs `claude -p "$SYMPHONY_PROMPT" --output-format stream-json`
    /// with optional model and max-turns flags.
    fn build_claude_command(&self, max_turns: u32) -> String {
        let mut cmd = self.profile.command.clone();
        cmd = format!("{cmd} -p \"$SYMPHONY_PROMPT\"");
        cmd = format!("{cmd} --output-format=stream-json");
        // Only skip permissions when approval_policy is "never" (default).
        // Other policies let Claude prompt for approval (not applicable in
        // headless mode, but avoids the dangerous flag when not intended).
        let skip_permissions = self.profile.approval_policy
            .as_deref()
            .map(|p| p == "never")
            .unwrap_or(true);
        if skip_permissions {
            cmd = format!("{cmd} --dangerously-skip-permissions");
        }
        cmd = format!("{cmd} --max-turns {max_turns}");
        cmd = format!("{cmd} --verbose");
        if let Some(ref model) = self.profile.model {
            cmd = format!("{cmd} --model {model}");
        }
        if let Some(ref effort) = self.profile.reasoning_effort {
            cmd = format!("{cmd} --effort {effort}");
        }
        cmd
    }

    /// Start a Claude CLI session. No handshake needed.
    ///
    /// Launches the `claude` CLI subprocess with the prompt passed via
    /// the `SYMPHONY_PROMPT` environment variable to avoid shell escaping
    /// issues. Returns an `AgentSession` ready for streaming.
    pub async fn start_claude_session(
        &self,
        workspace_path: &Path,
        issue: &Issue,
        prompt: &str,
        max_turns: u32,
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<AgentSession, SymphonyError> {
        let command = self.build_claude_command(max_turns);

        info!(
            issue_id = %issue.id,
            command = %command,
            cwd = %workspace_path.display(),
            "starting Claude CLI session"
        );

        // Pass prompt via env var to avoid shell escaping issues.
        let mut env_vars = self.build_env_vars();
        env_vars.push(("SYMPHONY_PROMPT".to_string(), prompt.to_string()));
        let env_refs: Vec<(&str, &str)> =
            env_vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();

        // Do not merge stderr for Claude CLI (stream-json goes to stdout,
        // verbose logs go to stderr).
        let process = match AgentProcess::launch(
            &command, workspace_path, &env_refs, false,
        ).await {
            Ok(p) => p,
            Err(e) => {
                error!(error = %e, "failed to launch Claude CLI process");
                let _ = event_tx
                    .send(AgentEvent::StartupFailed {
                        error: e.to_string(),
                        timestamp: Utc::now(),
                    })
                    .await;
                return Err(e);
            }
        };

        let session_id = format!(
            "claude-{:x}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        );
        let session_info = SessionInfo {
            thread_id: session_id.clone(),
            turn_id: "1".to_string(),
            session_id: session_id.clone(),
        };

        let pid = process.pid().map(|p| p.to_string());

        let _ = event_tx
            .send(AgentEvent::SessionStarted {
                session_id: session_id.clone(),
                thread_id: session_id.clone(),
                turn_id: "1".to_string(),
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

    /// Run the entire Claude CLI session.
    ///
    /// This is a single blocking call -- Claude CLI manages its own turn
    /// loop internally. No multi-turn loop, no approval handler, and no
    /// continuation prompts are needed.
    pub async fn run_claude_session(
        &self,
        session: &mut AgentSession,
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<TurnResult, SymphonyError> {
        let timeout = self.build_timeout_config();
        claude_cli::stream_claude_session(
            &mut session.process,
            event_tx,
            timeout.turn_timeout,
        )
        .await
    }
}
