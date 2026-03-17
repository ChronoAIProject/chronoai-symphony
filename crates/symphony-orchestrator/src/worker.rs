//! Worker task that drives the agent through a multi-turn session.
//!
//! Each worker prepares a workspace, runs hooks, starts an agent session,
//! and loops through turns until the issue is resolved, the maximum number
//! of turns is reached, or an error occurs.

use std::sync::Arc;

use symphony_agent::protocol::events::AgentEvent;
use symphony_agent::protocol::streaming::TurnResult;
use symphony_agent::runner::AgentRunner;
use symphony_core::domain::{Issue, ServiceConfig};
use symphony_core::identifiers::normalize_state;
use symphony_tracker::traits::IssueTracker;
use symphony_workflow::template::render_prompt;
use symphony_workspace::manager::WorkspaceManager;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use crate::approval_queue::{PendingApprovalQueue, QueuedApprovalHandler};
use crate::events::WorkerExitReason;

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
    agent_runner: Arc<AgentRunner>,
    tracker: Arc<dyn IssueTracker>,
    workspace_manager: Arc<WorkspaceManager>,
    event_tx: mpsc::Sender<(String, AgentEvent)>,
    approval_queue: Arc<PendingApprovalQueue>,
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
) -> WorkerResult {
    let issue_id = issue.id.clone();
    let identifier = issue.identifier.clone();
    let max_turns = config.agent_max_turns;

    info!(
        issue_id = %issue_id,
        identifier = %issue.identifier,
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
                issue_id,
                exit_reason: WorkerExitReason::Abnormal(format!("workspace error: {e}")),
            };
        }
    };
    let workspace_path = workspace.path;

    // Step 2: Render prompt template with issue data.
    let rendered_prompt = match render_prompt(&prompt_template, &issue, attempt) {
        Ok(p) => {
            info!(issue_id = %issue_id, prompt_len = p.len(), "prompt rendered");
            p
        }
        Err(e) => {
            error!(issue_id = %issue_id, error = %e, "failed to render prompt template");
            return WorkerResult {
                issue_id,
                exit_reason: WorkerExitReason::Abnormal(format!("prompt render failed: {e}")),
            };
        }
    };

    // Step 3: Run before_run hook.
    if let Err(e) = workspace_manager.run_before_run_hook(&workspace_path, Some(&issue_id), Some(&identifier)).await {
        error!(issue_id = %issue_id, error = %e, "before_run hook failed");
        return WorkerResult {
            issue_id,
            exit_reason: WorkerExitReason::Abnormal(format!("before_run hook failed: {e}")),
        };
    }

    // Create a per-issue event sender that tags events with the issue ID.
    let (local_tx, mut local_rx) = mpsc::channel::<AgentEvent>(64);

    // Forward events with issue ID tagging.
    let forward_tx = event_tx.clone();
    let forward_issue_id = issue_id.clone();
    tokio::spawn(async move {
        while let Some(event) = local_rx.recv().await {
            let _ = forward_tx.send((forward_issue_id.clone(), event)).await;
        }
    });

    // Step 3: Start agent session.
    let mut session = match agent_runner
        .start_session(&workspace_path, &issue, &rendered_prompt, &local_tx)
        .await
    {
        Ok(session) => session,
        Err(e) => {
            error!(issue_id = %issue_id, error = %e, "failed to start session");
            workspace_manager.run_after_run_hook(&workspace_path, Some(&issue_id), Some(&identifier)).await;
            return WorkerResult {
                issue_id,
                exit_reason: WorkerExitReason::Abnormal(format!("session start failed: {e}")),
            };
        }
    };

    // Create a queued approval handler for this worker.
    let approval_handler = QueuedApprovalHandler::new(
        approval_queue,
        issue_id.clone(),
        issue.identifier.clone(),
    );

    // Step 4: Turn loop.
    let mut turn_count = 0u32;
    let mut exit_reason = WorkerExitReason::Normal;

    for turn_num in 0..max_turns {
        // Check if we've been cancelled (e.g., stall detection).
        if *cancel_rx.borrow() {
            info!(issue_id = %issue_id, "worker cancelled by orchestrator");
            exit_reason = WorkerExitReason::Abnormal("cancelled by orchestrator".to_string());
            break;
        }

        let is_first_turn = turn_num == 0;
        let prompt = if is_first_turn {
            rendered_prompt.clone()
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
                &issue,
                is_first_turn,
                &local_tx,
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
        // Stop if the issue has moved to a terminal state OR a handoff
        // state like "Human Review" where the agent should not continue.
        if turn_num + 1 < max_turns {
            match tracker
                .fetch_issue_states_by_ids(&[issue_id.clone()])
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

                        // Stop if the issue moved to a "handoff" state
                        // where the agent should wait for human action.
                        // These are states like "Human Review" where the
                        // agent has finished its work and is waiting.
                        let handoff_states = ["human review", "human-review", "humanreview",
                                              "merging", "blocked"];
                        let is_handoff = handoff_states
                            .iter()
                            .any(|h| normalize_state(h) == normalized);

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

    // Step 5: Stop session.
    if let Err(e) = agent_runner.stop_session(&mut session).await {
        warn!(issue_id = %issue_id, error = %e, "failed to stop session cleanly");
    }

    // Step 6: Run after_run hook (best effort).
    workspace_manager.run_after_run_hook(&workspace_path, Some(&issue_id), Some(&identifier)).await;

    info!(
        issue_id = %issue_id,
        turns = turn_count,
        exit_reason = ?exit_reason,
        "worker finished"
    );

    WorkerResult {
        issue_id,
        exit_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_continuation_prompt_is_not_empty() {
        assert!(!DEFAULT_CONTINUATION_PROMPT.is_empty());
    }
}
