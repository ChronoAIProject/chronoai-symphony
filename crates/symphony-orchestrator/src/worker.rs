//! Worker task that drives the agent through a multi-turn session.
//!
//! Each worker prepares a workspace, runs hooks, starts an agent session,
//! and loops through turns until the issue is resolved, the maximum number
//! of turns is reached, or an error occurs.

use std::sync::Arc;

use symphony_agent::protocol::events::AgentEvent;
use symphony_agent::protocol::streaming::TurnResult;
use symphony_agent::runner::AgentRunner;
use symphony_core::domain::{AgentType, Issue, ServiceConfig};
use symphony_core::identifiers::normalize_state;
use symphony_tracker::traits::IssueTracker;
use symphony_workflow::template::{render_prompt, render_prompt_with_stage, StageContext};
use symphony_workspace::manager::WorkspaceManager;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::approval_queue::{PendingApprovalQueue, QueuedApprovalHandler};
use crate::events::WorkerExitReason;

// Re-export so the orchestrator can still import from this module.
pub use symphony_agent::runner::AgentRunner as AgentRunnerType;

/// Default continuation prompt for subsequent turns.
const DEFAULT_CONTINUATION_PROMPT: &str =
    "Continue working on the issue. Review your previous changes and verify correctness.";

/// Result returned by a worker task.
#[derive(Debug)]
pub struct WorkerResult {
    pub issue_id: String,
    pub exit_reason: WorkerExitReason,
}

/// Run a complete worker session for a single issue.
///
/// This is the top-level async function spawned as a Tokio task by the
/// orchestrator. It manages the full lifecycle:
///
/// 1. Prepare workspace via WorkspaceManager
/// 2. Run before_run hook via WorkspaceManager
/// 3. Start agent session (launch process + handshake)
/// 4. Loop through turns (up to `max_turns`)
/// 5. Stop session
/// 6. Run after_run hook (best effort) via WorkspaceManager
/// 7. Return result
pub async fn run_worker(
    issue: Issue,
    attempt: Option<u32>,
    config: ServiceConfig,
    prompt_template: String,
    tracker: Arc<dyn IssueTracker>,
    workspace_manager: Arc<WorkspaceManager>,
    event_tx: mpsc::Sender<(String, AgentEvent)>,
    approval_queue: Arc<PendingApprovalQueue>,
    cancel_rx: tokio::sync::watch::Receiver<bool>,
    stage_role: Option<String>,
    scope: Option<String>,
) -> WorkerResult {
    let issue_id = issue.id.clone();
    let identifier = issue.identifier.clone();
    let max_turns = config.agent_max_turns;

    // Construct the issue_id used for events and running map key; if running
    // under a stage role, use the compound key so events route correctly.
    let event_issue_id = if let Some(ref role) = stage_role {
        format!("{}:{}", issue_id, role)
    } else {
        issue_id.clone()
    };

    // Resolve which agent profile to use for this issue.
    let profile = config.resolve_agent_for_issue(&issue).clone();
    let agent_type = profile.agent_type.clone();
    let profile_max_turns = profile.max_turns;
    let agent_runner = AgentRunner::new(profile);

    info!(
        issue_id = %issue_id,
        identifier = %issue.identifier,
        stage_role = ?stage_role,
        attempt = ?attempt,
        max_turns,
        "worker starting"
    );

    // Step 1: Prepare workspace.
    let workspace = match workspace_manager.create_for_issue(&issue.identifier).await {
        Ok(ws) => ws,
        Err(e) => {
            error!(issue_id = %issue_id, error = %e, "failed to prepare workspace");
            return WorkerResult {
                issue_id: event_issue_id,
                exit_reason: WorkerExitReason::Abnormal(format!("workspace error: {e}")),
            };
        }
    };
    let workspace_path = workspace.path;

    // Step 2: Render prompt template with issue data and optional stage context.
    let stage = config.resolve_stage_for_issue(&issue);
    let rendered_prompt = {
        let result = if let Some(s) = &stage {
            if let Some(ref stage_prompt) = s.prompt {
                // Two-pass: render default prompt first, then render stage
                // prompt with the default prompt available as {{ default_prompt }}.
                let default_rendered = render_prompt(&prompt_template, &issue, attempt)
                    .unwrap_or_default();
                let ctx = StageContext {
                    role: s.role.clone(),
                    transition_to: s.transition_to.clone(),
                    reject_to: s.reject_to.clone(),
                    default_prompt: default_rendered,
                };
                render_prompt_with_stage(stage_prompt, &issue, attempt, Some(&ctx))
            } else {
                // No custom prompt, but add stage vars to default prompt.
                let ctx = StageContext {
                    role: s.role.clone(),
                    transition_to: s.transition_to.clone(),
                    reject_to: s.reject_to.clone(),
                    default_prompt: String::new(),
                };
                render_prompt_with_stage(&prompt_template, &issue, attempt, Some(&ctx))
            }
        } else {
            render_prompt(&prompt_template, &issue, attempt)
        };

        match result {
            Ok(p) => {
                info!(issue_id = %issue_id, prompt_len = p.len(), "prompt rendered");
                p
            }
            Err(e) => {
                error!(issue_id = %issue_id, error = %e, "failed to render prompt template");
                return WorkerResult {
                    issue_id: event_issue_id,
                    exit_reason: WorkerExitReason::Abnormal(format!("prompt render failed: {e}")),
                };
            }
        }
    };

    // Step 2a: Append scope hint to the prompt if provided.
    let rendered_prompt = if let Some(ref scope_dir) = scope {
        format!(
            "{}\n\n## Scope\n\nFocus your changes on the `{}` directory. \
             Other agents may be working on other parts of the codebase simultaneously.\n",
            rendered_prompt, scope_dir
        )
    } else {
        rendered_prompt
    };

    // Step 2b: Set git identity in the workspace if configured.
    if let Some(ref name) = config.git_user_name {
        let _ = tokio::process::Command::new("git")
            .args(["config", "user.name", name])
            .current_dir(&workspace_path)
            .output()
            .await;
    }
    if let Some(ref email) = config.git_user_email {
        let _ = tokio::process::Command::new("git")
            .args(["config", "user.email", email])
            .current_dir(&workspace_path)
            .output()
            .await;
    }

    // Step 3: Run before_run hook.
    if let Err(e) = workspace_manager.run_before_run_hook(&workspace_path, Some(&issue_id), Some(&identifier)).await {
        error!(issue_id = %issue_id, error = %e, "before_run hook failed");
        return WorkerResult {
            issue_id: event_issue_id,
            exit_reason: WorkerExitReason::Abnormal(format!("before_run hook failed: {e}")),
        };
    }

    // Create a per-issue event sender that tags events with the issue ID
    // (or compound key for parallel stages).
    let (local_tx, mut local_rx) = mpsc::channel::<AgentEvent>(64);

    // Forward events with issue ID tagging.
    let forward_tx = event_tx.clone();
    let forward_issue_id = event_issue_id.clone();
    tokio::spawn(async move {
        while let Some(event) = local_rx.recv().await {
            let _ = forward_tx.send((forward_issue_id.clone(), event)).await;
        }
    });

    // Branch on agent type.
    let exit_reason = match agent_type {
        AgentType::ClaudeCli => {
            let effective_max_turns = profile_max_turns.unwrap_or(max_turns);
            run_claude_worker(
                &agent_runner,
                &issue,
                &issue_id,
                &identifier,
                &rendered_prompt,
                effective_max_turns,
                &workspace_path,
                &local_tx,
                &cancel_rx,
            )
            .await
        }
        AgentType::Codex => {
            run_codex_worker(
                &agent_runner,
                &issue,
                &issue_id,
                &identifier,
                &rendered_prompt,
                max_turns,
                &workspace_path,
                &local_tx,
                &cancel_rx,
                &config,
                &tracker,
                &approval_queue,
            )
            .await
        }
    };

    // Run after_run hook (best effort).
    workspace_manager.run_after_run_hook(&workspace_path, Some(&issue_id), Some(&identifier)).await;

    info!(
        issue_id = %issue_id,
        exit_reason = ?exit_reason,
        "worker finished"
    );

    WorkerResult {
        issue_id: event_issue_id,
        exit_reason,
    }
}

/// Run a Claude CLI worker session.
///
/// Single subprocess invocation -- Claude manages its own turn loop.
/// No approval handler, no between-turn issue state checks.
async fn run_claude_worker(
    agent_runner: &AgentRunner,
    issue: &Issue,
    issue_id: &str,
    _identifier: &str,
    prompt: &str,
    max_turns: u32,
    workspace_path: &std::path::Path,
    event_tx: &mpsc::Sender<AgentEvent>,
    cancel_rx: &tokio::sync::watch::Receiver<bool>,
) -> WorkerExitReason {
    if *cancel_rx.borrow() {
        info!(issue_id = %issue_id, "worker cancelled before start");
        return WorkerExitReason::Abnormal("cancelled by orchestrator".to_string());
    }

    let mut session = match agent_runner
        .start_claude_session(workspace_path, issue, prompt, max_turns, event_tx)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            error!(issue_id = %issue_id, error = %e, "failed to start Claude session");
            return WorkerExitReason::Abnormal(format!("claude session start failed: {e}"));
        }
    };

    let turn_result = match agent_runner.run_claude_session(&mut session, event_tx).await {
        Ok(result) => result,
        Err(e) => {
            error!(issue_id = %issue_id, error = %e, "Claude session error");
            let _ = agent_runner.stop_session(&mut session).await;
            return WorkerExitReason::Abnormal(format!("claude session error: {e}"));
        }
    };

    let _ = agent_runner.stop_session(&mut session).await;

    match turn_result {
        TurnResult::Completed => {
            info!(issue_id = %issue_id, "Claude session completed");
            WorkerExitReason::Normal
        }
        TurnResult::Failed(error) => {
            warn!(issue_id = %issue_id, error = %error, "Claude session failed");
            WorkerExitReason::Abnormal(format!("claude session failed: {error}"))
        }
        TurnResult::TimedOut => {
            warn!(issue_id = %issue_id, "Claude session timed out");
            WorkerExitReason::Abnormal("claude session timed out".to_string())
        }
        TurnResult::ProcessExited => {
            warn!(issue_id = %issue_id, "Claude process exited");
            WorkerExitReason::Abnormal("claude process exited unexpectedly".to_string())
        }
        other => {
            warn!(issue_id = %issue_id, result = ?other, "Claude session ended");
            WorkerExitReason::Normal
        }
    }
}

/// Run a Codex worker session with multi-turn loop.
///
/// Extracted from the original worker logic for the Codex JSON-RPC protocol.
#[allow(clippy::too_many_arguments)]
async fn run_codex_worker(
    agent_runner: &AgentRunner,
    issue: &Issue,
    issue_id: &str,
    identifier: &str,
    rendered_prompt: &str,
    max_turns: u32,
    workspace_path: &std::path::Path,
    local_tx: &mpsc::Sender<AgentEvent>,
    cancel_rx: &tokio::sync::watch::Receiver<bool>,
    config: &ServiceConfig,
    tracker: &Arc<dyn IssueTracker>,
    approval_queue: &Arc<PendingApprovalQueue>,
) -> WorkerExitReason {
    let mut session = match agent_runner
        .start_session(workspace_path, issue, rendered_prompt, local_tx)
        .await
    {
        Ok(session) => session,
        Err(e) => {
            error!(issue_id = %issue_id, error = %e, "failed to start session");
            return WorkerExitReason::Abnormal(format!("session start failed: {e}"));
        }
    };

    // Create a queued approval handler for this worker.
    let approval_handler = QueuedApprovalHandler::new(
        Arc::clone(approval_queue),
        issue_id.to_string(),
        identifier.to_string(),
    );

    let mut turn_count = 0u32;
    let mut exit_reason = WorkerExitReason::Normal;

    for turn_num in 0..max_turns {
        if *cancel_rx.borrow() {
            info!(issue_id = %issue_id, "worker cancelled by orchestrator");
            exit_reason = WorkerExitReason::Abnormal("cancelled by orchestrator".to_string());
            break;
        }

        let is_first_turn = turn_num == 0;
        let prompt = if is_first_turn {
            rendered_prompt.to_string()
        } else {
            DEFAULT_CONTINUATION_PROMPT.to_string()
        };

        info!(
            issue_id = %issue_id,
            turn = turn_num + 1,
            max_turns,
            "starting turn"
        );

        let turn_result = match agent_runner
            .run_turn(
                &mut session,
                &prompt,
                issue,
                is_first_turn,
                local_tx,
                &approval_handler,
            )
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(issue_id = %issue_id, error = %e, "turn execution error");
                exit_reason =
                    WorkerExitReason::Abnormal(format!("turn execution error: {e}"));
                break;
            }
        };

        turn_count += 1;

        match turn_result {
            TurnResult::Completed => {
                info!(issue_id = %issue_id, turn = turn_count, "turn completed");
            }
            TurnResult::Failed(ref error) => {
                warn!(issue_id = %issue_id, error = %error, "turn failed");
                exit_reason = WorkerExitReason::Abnormal(format!("turn failed: {error}"));
                break;
            }
            TurnResult::Cancelled => {
                info!(issue_id = %issue_id, "turn cancelled");
                exit_reason = WorkerExitReason::Abnormal("turn cancelled".to_string());
                break;
            }
            TurnResult::TimedOut => {
                warn!(issue_id = %issue_id, "turn timed out");
                exit_reason = WorkerExitReason::Abnormal("turn timed out".to_string());
                break;
            }
            TurnResult::ProcessExited => {
                warn!(issue_id = %issue_id, "agent process exited");
                exit_reason =
                    WorkerExitReason::Abnormal("agent process exited unexpectedly".to_string());
                break;
            }
            TurnResult::InputRequired => {
                warn!(issue_id = %issue_id, "agent requires user input");
                exit_reason =
                    WorkerExitReason::Abnormal("agent requires user input".to_string());
                break;
            }
        }

        // Check issue state via tracker before next turn.
        if turn_num + 1 < max_turns {
            match tracker
                .fetch_issue_states_by_ids(&[issue_id.to_string()])
                .await
            {
                Ok(issues) => {
                    if let Some(updated) = issues.first() {
                        let normalized = normalize_state(&updated.state);
                        let is_terminal = config
                            .tracker_terminal_states
                            .iter()
                            .any(|t| normalize_state(t) == normalized);

                        if is_terminal {
                            info!(
                                issue_id = %issue_id,
                                state = %updated.state,
                                "issue reached terminal state, stopping"
                            );
                            break;
                        }

                        let is_handoff = if config.pipeline_stages.is_empty() {
                            // Legacy hardcoded list.
                            let handoff_states = [
                                "human review", "human-review", "humanreview",
                                "code review", "code-review", "codereview",
                                "merging", "blocked",
                            ];
                            handoff_states
                                .iter()
                                .any(|h| normalize_state(h) == normalized)
                        } else {
                            config.is_no_agent_state_by_name(&updated.state)
                        };

                        if is_handoff {
                            info!(
                                issue_id = %issue_id,
                                state = %updated.state,
                                "issue moved to handoff state, stopping worker"
                            );
                            break;
                        }
                    } else {
                        warn!(
                            issue_id = %issue_id,
                            "issue not found in tracker, stopping"
                        );
                        break;
                    }
                }
                Err(e) => {
                    warn!(
                        issue_id = %issue_id,
                        error = %e,
                        "failed to check issue state, continuing"
                    );
                }
            }
        }
    }

    if let Err(e) = agent_runner.stop_session(&mut session).await {
        warn!(issue_id = %issue_id, error = %e, "failed to stop session cleanly");
    }

    info!(
        issue_id = %issue_id,
        turns = turn_count,
        exit_reason = ?exit_reason,
        "codex worker finished"
    );

    exit_reason
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_continuation_prompt_is_not_empty() {
        assert!(!DEFAULT_CONTINUATION_PROMPT.is_empty());
    }
}
