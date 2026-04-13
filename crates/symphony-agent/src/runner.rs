//! Agent runner -- manages process lifecycle and turn execution.
//!
//! The `AgentRunner` launches agent processes, performs the handshake,
//! and provides a turn-by-turn execution interface. The orchestrator's
//! worker task calls into the runner in a loop, checking issue state
//! between turns.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::{Value, json};
use symphony_core::domain::{AgentProfileConfig, Issue};
use symphony_core::error::SymphonyError;
use tokio::sync::{mpsc, watch};
use tracing::{error, info};

use crate::approval_handler::ApprovalHandler;
use crate::process::AgentProcess;
use crate::protocol::claude_cli;
use crate::protocol::dynamic_tools::{
    DynamicToolContext, context_from_env_vars, coordination_tool_specs,
};
use crate::protocol::events::AgentEvent;
use crate::protocol::handshake::{SessionInfo, perform_handshake};
use crate::protocol::messages::{
    build_turn_start, default_approval_policy, default_thread_sandbox, default_turn_sandbox_policy,
};
use crate::protocol::streaming::{TurnResult, stream_turn};
use crate::timeout::TimeoutConfig;

/// A live agent session with an active subprocess and thread context.
pub struct AgentSession {
    pub process: AgentProcess,
    pub session_info: SessionInfo,
    pub workspace_path: PathBuf,
    pub dynamic_tool_context: Option<DynamicToolContext>,
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

fn shell_escape_arg(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    if value.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                '_' | '-' | '.' | '/' | ':' | '=' | ',' | '@' | '(' | ')' | '*'
            )
    }) {
        return value.to_string();
    }

    format!("'{}'", value.replace('\'', "'\"'\"'"))
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
            Some(s) if s.trim() == "danger-full-access" => {
                json!({"type": "dangerFullAccess"})
            }
            Some(s) => serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.clone())),
            None => match self.profile.thread_sandbox.as_deref().map(str::trim) {
                // Keep full-access sessions actually full-access across both
                // thread/start and turn/start unless the workflow overrides the
                // turn sandbox policy explicitly.
                // Codex v0.118.0+ requires the tagged enum format for turn/start
                // sandboxPolicy: {"type": "dangerFullAccess"} instead of the
                // plain string "danger-full-access" (which thread/start still accepts).
                Some("danger-full-access") => json!({"type": "dangerFullAccess"}),
                _ => default_turn_sandbox_policy(workspace_path),
            },
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
    fn build_env_vars(&self, extra_env_vars: &[(String, String)]) -> Vec<(String, String)> {
        let mut vars = Vec::new();
        if let Some(ref model) = self.profile.model {
            vars.push(("SYMPHONY_AGENT_MODEL".to_string(), model.clone()));
        }
        if let Some(ref effort) = self.profile.reasoning_effort {
            vars.push(("MODEL_REASONING_EFFORT".to_string(), effort.clone()));
            vars.push(("SYMPHONY_REASONING_EFFORT".to_string(), effort.clone()));
        }

        // Always pass GH_TOKEN to the agent subprocess so `gh` uses the
        // Symphony-configured token instead of the user's personal keyring.
        // Priority: token file (GitHub App, refreshed) > env var (PAT, static).
        if let Ok(token_file) = std::env::var("SYMPHONY_TOKEN_FILE") {
            vars.push(("SYMPHONY_TOKEN_FILE".to_string(), token_file.clone()));
            if let Ok(fresh_token) = std::fs::read_to_string(&token_file) {
                let token = fresh_token.trim().to_string();
                if !token.is_empty() {
                    vars.push(("GH_TOKEN".to_string(), token.clone()));
                    vars.push(("GITHUB_TOKEN".to_string(), token));
                }
            }
        } else if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            // PAT auth: pass the token explicitly so gh uses it
            // instead of the user's personal keyring credentials.
            if !token.is_empty() {
                vars.push(("GH_TOKEN".to_string(), token.clone()));
                vars.push(("GITHUB_TOKEN".to_string(), token));
            }
        }
        if let Ok(url) = std::env::var("SYMPHONY_COORDINATION_API_URL") {
            if !url.is_empty() {
                vars.push(("SYMPHONY_COORDINATION_API_URL".to_string(), url));
            }
        }

        vars.extend(extra_env_vars.iter().cloned());

        vars
    }

    /// Start a new agent session: launch process and perform handshake.
    pub async fn start_session(
        &self,
        workspace_path: &Path,
        issue: &Issue,
        prompt: &str,
        extra_env_vars: &[(String, String)],
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

        let env_vars = self.build_env_vars(extra_env_vars);
        let env_refs: Vec<(&str, &str)> = env_vars
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let dynamic_tool_context = context_from_env_vars(workspace_path, &env_vars);
        let dynamic_tools = if dynamic_tool_context.is_some() {
            coordination_tool_specs()
        } else {
            Vec::new()
        };
        let mut process =
            match AgentProcess::launch(&command, workspace_path, &env_refs, true).await {
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
            &dynamic_tools,
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
            dynamic_tool_context,
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
        cancel_rx: &mut watch::Receiver<bool>,
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
            let turn_json =
                serde_json::to_string(&turn_req).map_err(|e| SymphonyError::ResponseError {
                    detail: format!("failed to serialize turn/start: {e}"),
                })?;
            session.process.write_message(&turn_json).await?;
        }

        stream_turn(
            &mut session.process,
            event_tx,
            timeout_config.turn_timeout,
            approval_handler,
            session.dynamic_tool_context.as_ref(),
            cancel_rx,
        )
        .await
    }

    /// Stop an active session by killing the agent process.
    pub async fn stop_session(&self, session: &mut AgentSession) -> Result<(), SymphonyError> {
        info!(
            session_id = %session.session_info.session_id,
            "stopping agent session"
        );
        if session.process.try_wait().await?.is_some() {
            return Ok(());
        }
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
    /// Constructs `claude -p "$(cat "$SYMPHONY_PROMPT_FILE")" --output-format stream-json`
    /// with Chrono Code-compatible streaming flags and optional filters.
    fn build_claude_command(&self, max_turns: u32) -> String {
        let mut cmd = self.profile.command.clone();
        // Read prompt from file to avoid bash expanding $, backticks, etc.
        // in the prompt content. $(cat ...) output is not re-expanded
        // when inside double quotes.
        cmd = format!("{cmd} -p \"$(cat \"$SYMPHONY_PROMPT_FILE\")\"");
        cmd = format!("{cmd} --output-format=stream-json");
        cmd = format!("{cmd} --include-hook-events");
        // Only skip permissions when approval_policy is "never" (default).
        // Other policies let Claude prompt for approval (not applicable in
        // headless mode, but avoids the dangerous flag when not intended).
        let skip_permissions = self
            .profile
            .approval_policy
            .as_deref()
            .map(|p| p == "never")
            .unwrap_or(true);
        if skip_permissions {
            cmd = format!("{cmd} --dangerously-skip-permissions");
        }
        cmd = format!("{cmd} --max-turns {max_turns}");
        cmd = format!("{cmd} --verbose");
        if !self.profile.allowed_tools.is_empty() {
            let tools = shell_escape_arg(&self.profile.allowed_tools.join(","));
            cmd = format!("{cmd} --allowed-tools {tools}");
        }
        if !self.profile.disallowed_tools.is_empty() {
            let tools = shell_escape_arg(&self.profile.disallowed_tools.join(","));
            cmd = format!("{cmd} --disallowed-tools {tools}");
        }
        if let Some(ref model) = self.profile.model {
            cmd = format!("{cmd} --model {}", shell_escape_arg(model));
        }
        if let Some(ref effort) = self.profile.reasoning_effort {
            cmd = format!("{cmd} --effort {}", shell_escape_arg(effort));
        }
        cmd
    }

    /// Start a Claude CLI session. No handshake needed.
    ///
    /// Launches the `claude` CLI subprocess with the prompt written to
    /// `SYMPHONY_PROMPT_FILE` to avoid shell escaping issues. Returns an
    /// `AgentSession` ready for streaming.
    pub async fn start_claude_session(
        &self,
        workspace_path: &Path,
        issue: &Issue,
        prompt: &str,
        max_turns: u32,
        extra_env_vars: &[(String, String)],
        event_tx: &mpsc::Sender<AgentEvent>,
    ) -> Result<AgentSession, SymphonyError> {
        let command = self.build_claude_command(max_turns);

        info!(
            issue_id = %issue.id,
            command = %command,
            cwd = %workspace_path.display(),
            "starting Claude CLI session"
        );

        // Write prompt to a file to avoid bash expanding $, backticks, etc.
        // The command reads it via $(cat "$SYMPHONY_PROMPT_FILE").
        let prompt_file = workspace_path.join(".symphony_prompt");
        std::fs::write(&prompt_file, prompt).map_err(|e| SymphonyError::ResponseError {
            detail: format!("failed to write prompt file: {e}"),
        })?;

        let mut env_vars = self.build_env_vars(extra_env_vars);
        env_vars.push((
            "SYMPHONY_PROMPT_FILE".to_string(),
            prompt_file.to_string_lossy().to_string(),
        ));
        let env_refs: Vec<(&str, &str)> = env_vars
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        // Do not merge stderr for Claude CLI (stream-json goes to stdout,
        // verbose logs go to stderr).
        let process = match AgentProcess::launch(&command, workspace_path, &env_refs, false).await {
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
            dynamic_tool_context: None,
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
        cancel_rx: &mut watch::Receiver<bool>,
    ) -> Result<TurnResult, SymphonyError> {
        let timeout = self.build_timeout_config();
        claude_cli::stream_claude_session(
            &mut session.process,
            event_tx,
            timeout.turn_timeout,
            cancel_rx,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use symphony_core::domain::config::AgentType;

    fn codex_profile() -> AgentProfileConfig {
        AgentProfileConfig {
            agent_type: AgentType::Codex,
            command: "codex app-server".to_string(),
            approval_policy: Some("never".to_string()),
            thread_sandbox: Some("danger-full-access".to_string()),
            turn_sandbox_policy: None,
            turn_timeout_ms: 3_600_000,
            read_timeout_ms: 30_000,
            stall_timeout_ms: 300_000,
            model: Some("gpt-5.3-codex".to_string()),
            reasoning_effort: Some("xhigh".to_string()),
            network_access: true,
            max_turns: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
        }
    }

    fn claude_profile() -> AgentProfileConfig {
        AgentProfileConfig {
            agent_type: AgentType::ClaudeCli,
            command: "claude".to_string(),
            approval_policy: Some("never".to_string()),
            thread_sandbox: None,
            turn_sandbox_policy: None,
            turn_timeout_ms: 7_200_000,
            read_timeout_ms: 30_000,
            stall_timeout_ms: 600_000,
            model: Some("claude-sonnet-4-6".to_string()),
            reasoning_effort: Some("high".to_string()),
            network_access: true,
            max_turns: Some(20),
            allowed_tools: vec!["Read".to_string(), "Bash(gh pr:*)".to_string()],
            disallowed_tools: vec!["Edit".to_string()],
        }
    }

    #[test]
    fn shell_escape_quotes_shell_sensitive_values() {
        assert_eq!(
            shell_escape_arg("Bash(git commit -m 'test')"),
            "'Bash(git commit -m '\"'\"'test'\"'\"')'"
        );
    }

    #[test]
    fn build_claude_command_includes_streaming_flags_and_tool_filters() {
        let runner = AgentRunner::new(claude_profile());

        let command = runner.build_claude_command(12);

        assert!(command.contains("-p \"$(cat \"$SYMPHONY_PROMPT_FILE\")\""));
        assert!(command.contains("--output-format=stream-json"));
        assert!(command.contains("--include-hook-events"));
        assert!(command.contains("--dangerously-skip-permissions"));
        assert!(command.contains("--max-turns 12"));
        assert!(command.contains("--verbose"));
        assert!(command.contains("--allowed-tools 'Read,Bash(gh pr:*)'"));
        assert!(command.contains("--disallowed-tools Edit"));
        assert!(command.contains("--model claude-sonnet-4-6"));
        assert!(command.contains("--effort high"));
    }

    #[test]
    fn build_claude_command_omits_permission_skip_when_not_never() {
        let mut profile = claude_profile();
        profile.approval_policy = Some("on-request".to_string());
        profile.allowed_tools.clear();
        profile.disallowed_tools.clear();

        let runner = AgentRunner::new(profile);
        let command = runner.build_claude_command(3);

        assert!(!command.contains("--dangerously-skip-permissions"));
        assert!(!command.contains("--allowed-tools"));
        assert!(!command.contains("--disallowed-tools"));
    }

    #[test]
    fn danger_full_access_thread_sandbox_carries_into_turn_policy() {
        let runner = AgentRunner::new(codex_profile());
        let policy = runner.resolve_turn_sandbox_policy("/tmp/ws");

        // Codex v0.118.0+ requires the tagged enum format for turn/start.
        // networkAccess is injected from the profile config.
        assert_eq!(
            policy,
            json!({"type": "dangerFullAccess", "networkAccess": true})
        );
    }
}
