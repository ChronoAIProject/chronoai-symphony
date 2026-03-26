//! Main orchestrator event loop.
//!
//! Coordinates the lifecycle of agent workers: polling for new issues,
//! dispatching eligible issues to workers, reconciling running sessions,
//! managing retries, and responding to configuration changes.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use symphony_agent::protocol::events::AgentEvent;
use symphony_core::domain::{
    CodexTotals, Issue, OrchestratorState, PipelineStage, RunningEntry,
    ServiceConfig,
};
use symphony_tracker::traits::IssueTracker;
use symphony_workspace::manager::WorkspaceManager;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::activity_log::{ActivityEntry, ActivityLog};
use crate::approval_queue::PendingApprovalQueue;
use crate::cleanup::startup_terminal_cleanup;
use crate::dispatch::{is_dispatch_eligible, sort_for_dispatch};
use crate::events::{OrchestratorEvent, WorkerExitReason};
use crate::reconciliation::{reconcile_running_issues, ReconciliationAction};
use crate::retry::compute_backoff_ms;
use crate::worker::run_worker;

/// Snapshot of the orchestrator state for external consumption (status API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSnapshot {
    pub running_count: usize,
    pub running_issues: Vec<RunningIssueSummary>,
    pub retry_count: usize,
    pub completed_count: usize,
    pub codex_totals: CodexTotals,
    pub codex_rate_limits: Option<serde_json::Value>,
}

/// Summary of a running issue for the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningIssueSummary {
    pub issue_id: String,
    pub identifier: String,
    pub state: String,
    pub session_id: Option<String>,
    pub turn_count: u32,
    pub started_at: String,
}

/// Thread-safe handle for reading the latest orchestrator snapshot.
///
/// Shared with the HTTP server so the dashboard can display live state.
pub type SharedSnapshot = Arc<RwLock<serde_json::Value>>;

/// The main orchestrator that coordinates all agent activity.
pub struct Orchestrator {
    state: OrchestratorState,
    config: ServiceConfig,
    prompt_template: String,
    tracker: Arc<dyn IssueTracker>,
    workspace_manager: Arc<WorkspaceManager>,
    event_rx: mpsc::Receiver<OrchestratorEvent>,
    event_tx: mpsc::Sender<OrchestratorEvent>,
    shared_snapshot: SharedSnapshot,
    approval_queue: Arc<PendingApprovalQueue>,
    activity_log: Arc<ActivityLog>,
    cancel_senders: HashMap<String, tokio::sync::watch::Sender<bool>>,
}

impl Orchestrator {
    /// Create a new orchestrator with the given dependencies.
    pub fn new(
        config: ServiceConfig,
        prompt_template: String,
        tracker: Arc<dyn IssueTracker>,
        workspace_manager: Arc<WorkspaceManager>,
    ) -> Self {
        let state = OrchestratorState {
            poll_interval_ms: config.polling_interval_ms,
            max_concurrent_agents: config.agent_max_concurrent,
            running: HashMap::new(),
            claimed: HashSet::new(),
            retry_attempts: HashMap::new(),
            completed: HashSet::new(),
            codex_totals: CodexTotals::default(),
            codex_rate_limits: None,
        };

        let (event_tx, event_rx) = mpsc::channel(256);
        let shared_snapshot = Arc::new(RwLock::new(serde_json::json!({
            "generated_at": Utc::now().to_rfc3339(),
            "counts": { "running": 0, "retrying": 0 },
            "running": [],
            "retrying": [],
            "codex_totals": { "input_tokens": 0, "output_tokens": 0, "total_tokens": 0, "seconds_running": 0.0 },
            "rate_limits": null
        })));

        let approval_queue = Arc::new(PendingApprovalQueue::new());
        let activity_log = Arc::new(ActivityLog::new(200));

        Self {
            state,
            config,
            prompt_template,
            tracker,
            workspace_manager,
            event_rx,
            event_tx,
            shared_snapshot,
            approval_queue,
            activity_log,
            cancel_senders: HashMap::new(),
        }
    }

    /// Get a sender for posting events to the orchestrator.
    pub fn event_sender(&self) -> mpsc::Sender<OrchestratorEvent> {
        self.event_tx.clone()
    }

    /// Get a shared handle to the snapshot for the HTTP server.
    pub fn shared_snapshot(&self) -> SharedSnapshot {
        Arc::clone(&self.shared_snapshot)
    }

    /// Get the shared approval queue for the HTTP server.
    pub fn approval_queue(&self) -> Arc<PendingApprovalQueue> {
        Arc::clone(&self.approval_queue)
    }

    /// Get the shared activity log for the HTTP server.
    pub fn activity_log(&self) -> Arc<ActivityLog> {
        Arc::clone(&self.activity_log)
    }

    /// Get a snapshot of the current orchestrator state.
    pub fn get_snapshot(&self) -> OrchestratorSnapshot {
        let running_issues = self
            .state
            .running
            .iter()
            .map(|(issue_id, entry)| RunningIssueSummary {
                issue_id: issue_id.clone(),
                identifier: entry.identifier.clone(),
                state: entry.issue.state.clone(),
                session_id: entry.session_id.clone(),
                turn_count: entry.turn_count,
                started_at: entry.started_at.to_rfc3339(),
            })
            .collect();

        OrchestratorSnapshot {
            running_count: self.state.running.len(),
            running_issues,
            retry_count: self.state.retry_attempts.len(),
            completed_count: self.state.completed.len(),
            codex_totals: self.state.codex_totals.clone(),
            codex_rate_limits: self.state.codex_rate_limits.clone(),
        }
    }

    /// Run the orchestrator event loop.
    ///
    /// Performs startup cleanup, then enters a loop processing events
    /// from the channel until a Shutdown event is received.
    pub async fn run(&mut self) {
        info!("orchestrator starting");

        // Startup cleanup of terminal workspaces.
        startup_terminal_cleanup(
            self.tracker.as_ref(),
            self.workspace_manager.as_ref(),
            &self.config.tracker_terminal_states,
        )
        .await;

        // Spawn the tick timer.
        let tick_tx = self.event_tx.clone();
        let poll_interval = Duration::from_millis(self.state.poll_interval_ms);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(poll_interval);
            loop {
                interval.tick().await;
                if tick_tx.send(OrchestratorEvent::Tick).await.is_err() {
                    break;
                }
            }
        });

        // Channel for worker events tagged with issue ID.
        let (worker_event_tx, mut worker_event_rx) =
            mpsc::channel::<(String, AgentEvent)>(256);

        // Forward worker events into the orchestrator event loop.
        let codex_update_tx = self.event_tx.clone();
        tokio::spawn(async move {
            while let Some((issue_id, event)) = worker_event_rx.recv().await {
                let _ = codex_update_tx
                    .send(OrchestratorEvent::CodexUpdate { issue_id, event })
                    .await;
            }
        });

        // Main event loop.
        info!("orchestrator event loop started");
        self.publish_snapshot();
        while let Some(event) = self.event_rx.recv().await {
            match event {
                OrchestratorEvent::Tick => {
                    self.handle_tick(&worker_event_tx).await;
                }
                OrchestratorEvent::WorkerExited { issue_id, reason } => {
                    self.handle_worker_exited(&issue_id, reason);
                }
                OrchestratorEvent::CodexUpdate { issue_id, event } => {
                    self.handle_codex_update(&issue_id, event);
                }
                OrchestratorEvent::RetryTimerFired { issue_id } => {
                    self.handle_retry_timer(&issue_id, &worker_event_tx)
                        .await;
                }
                OrchestratorEvent::WorkflowReloaded {
                    config,
                    prompt_template,
                } => {
                    self.handle_workflow_reloaded(config, prompt_template);
                }
                OrchestratorEvent::RefreshRequested => {
                    info!("refresh requested, triggering immediate tick");
                    self.handle_tick(&worker_event_tx).await;
                }
                OrchestratorEvent::Shutdown => {
                    info!("shutdown requested, cleaning up");
                    self.handle_shutdown();
                    break;
                }
            }
            self.publish_snapshot();
        }

        info!("orchestrator event loop exited");
    }

    /// Handle a periodic tick: reconcile, fetch candidates, dispatch.
    async fn handle_tick(
        &mut self,
        worker_event_tx: &mpsc::Sender<(String, AgentEvent)>,
    ) {
        // Reconcile running issues.
        let actions = reconcile_running_issues(
            &self.state,
            self.tracker.as_ref(),
            self.workspace_manager.as_ref(),
            &self.config,
        )
        .await;

        for action in actions {
            self.apply_reconciliation_action(action).await;
        }

        // Fetch candidate issues from tracker.
        let candidates: Vec<Issue> = match self
            .tracker
            .fetch_candidate_issues()
            .await
        {
            Ok(issues) => issues,
            Err(e) => {
                warn!(error = %e, "failed to fetch issues from tracker");
                return;
            }
        };

        // Filter eligible and sort.
        let mut eligible: Vec<Issue> = candidates
            .into_iter()
            .filter(|issue| is_dispatch_eligible(issue, &self.state, &self.config))
            .collect();
        sort_for_dispatch(&mut eligible);

        // Dispatch eligible issues, with parallel-stage awareness.
        for issue in eligible {
            let stages = self.config.resolve_stages_for_issue(&issue);
            if stages.is_empty() {
                // No pipeline: use existing single-dispatch logic.
                if self.state.running.len() as u32 >= self.state.max_concurrent_agents {
                    break;
                }
                self.dispatch_issue(issue, None, worker_event_tx).await;
            } else {
                // Pipeline with stages: dispatch each unstarted stage.
                // But first, check if this issue has a worker running for a
                // DIFFERENT state (e.g., code-review worker still running but
                // issue moved to rework). Don't dispatch new state while old
                // state's worker is still active.
                let issue_state_norm = issue.state.to_lowercase().replace('-', " ");
                let has_worker_for_different_state = self.state.running.values().any(|entry| {
                    let raw_id = entry.issue.id.as_str();
                    raw_id == issue.id && {
                        // Compare against dispatched_state (immutable) not
                        // issue.state (updated by reconciliation).
                        let dispatched = entry.dispatched_state.to_lowercase().replace('-', " ");
                        dispatched != issue_state_norm
                    }
                });
                if has_worker_for_different_state {
                    debug!(
                        issue_id = %issue.id,
                        state = %issue.state,
                        "skipping dispatch: worker for previous state still running"
                    );
                    continue;
                }

                let stages_owned: Vec<_> = stages.into_iter().cloned().collect();
                for stage in &stages_owned {
                    if stage.agent == "none" {
                        continue;
                    }
                    let role = stage.role.as_deref().unwrap_or(&stage.agent);
                    let compound_key = format!("{}:{}", issue.id, role);

                    let already_running = self.state.running.values().any(|entry| {
                        entry.issue.id == issue.id
                            && entry.stage_role.as_deref() == Some(role)
                    });
                    if already_running {
                        continue;
                    }

                    // Don't re-dispatch a stage that already completed for
                    // this issue in this state cycle. This prevents finished
                    // parallel agents from being re-dispatched while their
                    // sibling is still running.
                    if self.state.completed.contains(&compound_key) {
                        debug!(
                            issue_id = %issue.id,
                            role = %role,
                            "skipping dispatch: stage already completed"
                        );
                        continue;
                    }
                    if self.state.running.len() as u32 >= self.state.max_concurrent_agents {
                        break;
                    }
                    self.dispatch_issue_with_stage(
                        issue.clone(),
                        None,
                        worker_event_tx,
                        stage,
                    )
                    .await;
                }
            }
        }
    }

    /// Apply a single reconciliation action.
    async fn apply_reconciliation_action(&mut self, action: ReconciliationAction) {
        match action {
            ReconciliationAction::TerminateAndCleanup { issue_id } => {
                info!(issue_id = %issue_id, "reconciliation: terminate and cleanup");
                if let Some(entry) = self.state.running.remove(&issue_id) {
                    self.state.completed.insert(issue_id.clone());
                    if let Err(e) = self
                        .workspace_manager
                        .cleanup_workspace(&entry.identifier)
                        .await
                    {
                        warn!(
                            issue_id = %issue_id,
                            error = %e,
                            "failed to cleanup workspace during reconciliation"
                        );
                    }
                }
            }
            ReconciliationAction::TerminateNoCleanup { issue_id } => {
                info!(issue_id = %issue_id, "reconciliation: terminate without cleanup");
                self.state.running.remove(&issue_id);
            }
            ReconciliationAction::UpdateSnapshot {
                issue_id,
                new_state,
            } => {
                if let Some(entry) = self.state.running.get_mut(&issue_id) {
                    entry.issue.state = new_state;
                }
            }
            ReconciliationAction::StallDetected { issue_id } => {
                warn!(issue_id = %issue_id, "stall detected, cancelling worker and scheduling retry");

                // Signal the worker to kill the agent process and exit.
                if let Some(cancel_tx) = self.cancel_senders.remove(&issue_id) {
                    let _ = cancel_tx.send(true);
                }

                // Treat stall like an abnormal exit: schedule a retry.
                if let Some(entry) = self.state.running.remove(&issue_id) {
                    self.state.claimed.remove(&issue_id);
                    self.approval_queue.remove_by_issue(&issue_id);
                    // Don't clear activity log so the UI preserves history.

                    let current_attempt = entry.retry_attempt.unwrap_or(0);
                    let next_attempt = current_attempt + 1;
                    let max_backoff = self.config.agent_max_retry_backoff_ms;
                    let delay_ms = compute_backoff_ms(next_attempt, max_backoff);

                    info!(
                        issue_id = %issue_id,
                        attempt = next_attempt,
                        delay_ms,
                        "scheduling stall retry"
                    );

                    self.state.retry_attempts.insert(
                        issue_id.clone(),
                        symphony_core::domain::RetryEntry {
                            issue_id: issue_id.clone(),
                            identifier: entry.identifier,
                            attempt: next_attempt,
                            due_at_ms: delay_ms,
                            error: Some("stall detected: no output within timeout".to_string()),
                        },
                    );

                    let retry_tx = self.event_tx.clone();
                    let retry_issue_id = issue_id.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        let _ = retry_tx
                            .send(OrchestratorEvent::RetryTimerFired {
                                issue_id: retry_issue_id,
                            })
                            .await;
                    });
                }
            }
        }
    }

    /// Dispatch a single issue to a worker task.
    async fn dispatch_issue(
        &mut self,
        issue: Issue,
        attempt: Option<u32>,
        worker_event_tx: &mpsc::Sender<(String, AgentEvent)>,
    ) {
        let issue_id = issue.id.clone();
        info!(
            issue_id = %issue_id,
            identifier = %issue.identifier,
            "dispatching issue to worker"
        );

        // Resolve agent type for this issue.
        let agent_type_str = self
            .config
            .resolve_agent_for_issue(&issue)
            .agent_type
            .to_string();

        // Add to running map.
        self.state.running.insert(
            issue_id.clone(),
            RunningEntry {
                identifier: issue.identifier.clone(),
                issue: issue.clone(),
                agent_type: agent_type_str,
                session_id: None,
                codex_app_server_pid: None,
                last_codex_message: None,
                last_codex_event: None,
                last_codex_timestamp: None,
                codex_input_tokens: 0,
                codex_output_tokens: 0,
                codex_total_tokens: 0,
                last_reported_input_tokens: 0,
                last_reported_output_tokens: 0,
                last_reported_total_tokens: 0,
                retry_attempt: attempt,
                stage_role: None,
                dispatched_state: issue.state.clone(),
                started_at: Utc::now(),
                turn_count: 0,
            },
        );
        // Create a cancellation channel for this worker.
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        self.state.claimed.insert(issue_id.clone());
        self.cancel_senders.insert(issue_id.clone(), cancel_tx);

        // Spawn worker task.
        let config = self.config.clone();
        let prompt_template = self.prompt_template.clone();
        let tracker = Arc::clone(&self.tracker);
        let wm = Arc::clone(&self.workspace_manager);
        let event_tx = worker_event_tx.clone();
        let orch_event_tx = self.event_tx.clone();
        let approval_queue = Arc::clone(&self.approval_queue);

        tokio::spawn(async move {
            let result = run_worker(
                issue,
                attempt,
                config,
                prompt_template,
                tracker,
                wm,
                event_tx,
                approval_queue,
                cancel_rx,
                None,
                None,
            )
            .await;

            let _ = orch_event_tx
                .send(OrchestratorEvent::WorkerExited {
                    issue_id: result.issue_id,
                    reason: result.exit_reason,
                })
                .await;
        });
    }

    /// Dispatch an issue to a worker task for a specific pipeline stage.
    ///
    /// Unlike `dispatch_issue`, this records the stage role in the running
    /// entry and passes stage-specific scope/prompt information to the worker.
    /// The running map key includes the role to allow multiple workers per issue.
    async fn dispatch_issue_with_stage(
        &mut self,
        issue: Issue,
        attempt: Option<u32>,
        worker_event_tx: &mpsc::Sender<(String, AgentEvent)>,
        stage: &PipelineStage,
    ) {
        let role = stage
            .role
            .as_deref()
            .unwrap_or(&stage.agent)
            .to_owned();
        // Use "issue_id:role" as the running map key to support parallel stages.
        let running_key = format!("{}:{}", issue.id, role);
        let issue_id = issue.id.clone();

        info!(
            issue_id = %issue_id,
            identifier = %issue.identifier,
            role = %role,
            "dispatching issue to worker (pipeline stage)"
        );

        // Resolve agent type from the stage's agent profile.
        let agent_type_str = self
            .config
            .get_agent_profile(&stage.agent)
            .map(|p| p.agent_type.to_string())
            .unwrap_or_else(|| {
                self.config
                    .resolve_agent_for_issue(&issue)
                    .agent_type
                    .to_string()
            });

        // Add to running map with compound key.
        self.state.running.insert(
            running_key.clone(),
            RunningEntry {
                identifier: issue.identifier.clone(),
                issue: issue.clone(),
                agent_type: agent_type_str,
                session_id: None,
                codex_app_server_pid: None,
                last_codex_message: None,
                last_codex_event: None,
                last_codex_timestamp: None,
                codex_input_tokens: 0,
                codex_output_tokens: 0,
                codex_total_tokens: 0,
                last_reported_input_tokens: 0,
                last_reported_output_tokens: 0,
                last_reported_total_tokens: 0,
                retry_attempt: attempt,
                stage_role: Some(role.clone()),
                dispatched_state: issue.state.clone(),
                started_at: Utc::now(),
                turn_count: 0,
            },
        );

        // Create a cancellation channel for this worker.
        let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
        self.state.claimed.insert(issue_id.clone());
        self.cancel_senders.insert(running_key.clone(), cancel_tx);

        // Spawn worker task.
        let config = self.config.clone();
        let prompt_template = self.prompt_template.clone();
        let tracker = Arc::clone(&self.tracker);
        let wm = Arc::clone(&self.workspace_manager);
        let event_tx = worker_event_tx.clone();
        let orch_event_tx = self.event_tx.clone();
        let approval_queue = Arc::clone(&self.approval_queue);
        let stage_role = Some(role);
        let scope = stage.scope.clone();

        tokio::spawn(async move {
            let result = run_worker(
                issue,
                attempt,
                config,
                prompt_template,
                tracker,
                wm,
                event_tx,
                approval_queue,
                cancel_rx,
                stage_role,
                scope,
            )
            .await;

            let _ = orch_event_tx
                .send(OrchestratorEvent::WorkerExited {
                    issue_id: result.issue_id,
                    reason: result.exit_reason,
                })
                .await;
        });
    }

    /// Handle a worker exiting: update state and schedule retry if needed.
    ///
    /// The `issue_id` may be a compound key `"issue_id:role"` for pipeline
    /// stages. The raw issue ID is extracted for the claimed set.
    fn handle_worker_exited(&mut self, issue_id: &str, reason: WorkerExitReason) {
        info!(
            issue_id = %issue_id,
            reason = ?reason,
            "worker exited"
        );

        // Extract the raw issue ID from a potential compound key "id:role".
        let raw_issue_id = issue_id
            .split(':')
            .next()
            .unwrap_or(issue_id);

        // Clean up cancellation sender.
        self.cancel_senders.remove(issue_id);

        // Persist the running entry's full token counts into the
        // cumulative totals. The snapshot adds running entries' tokens
        // live, so we store the FULL values here (not deltas) to ensure
        // the total doesn't drop when the entry is removed.
        if let Some(entry) = self.state.running.get(issue_id) {
            self.state.codex_totals.input_tokens += entry.codex_input_tokens;
            self.state.codex_totals.output_tokens += entry.codex_output_tokens;
            self.state.codex_totals.total_tokens += entry.codex_total_tokens;

            let elapsed = (Utc::now() - entry.started_at)
                .num_milliseconds()
                .max(0) as f64
                / 1000.0;
            self.state.codex_totals.seconds_running += elapsed;
        }

        // Clean up approval queue and activity log for this issue.
        self.approval_queue.remove_by_issue(issue_id);
        self.activity_log.remove_issue(issue_id);

        let entry = self.state.running.remove(issue_id);
        // Only remove from claimed if no other stages are still running for
        // this issue.
        let other_stages_running = self
            .state
            .running
            .values()
            .any(|e| e.issue.id == raw_issue_id);
        if !other_stages_running {
            self.state.claimed.remove(raw_issue_id);
        }

        match reason {
            WorkerExitReason::Normal => {
                info!(issue_id = %issue_id, "worker completed normally");
                self.state.completed.insert(issue_id.to_string());

                // Auto-transition: when ALL parallel stages for an issue finish
                // normally, move the issue to the next pipeline state.
                // Only auto-transition for single-outcome stages (no reject_to).
                // Stages with reject_to (e.g., code-review) have two possible
                // outcomes and must manage labels themselves.
                if !other_stages_running {
                    if let Some(ref entry_ref) = entry {
                        let stages = self.config.resolve_stages_for_issue(&entry_ref.issue);
                        // Find a matching stage that has transition_to but NO reject_to
                        let transition = stages.iter()
                            .find(|s| s.agent != "none" && s.reject_to.is_none())
                            .and_then(|s| s.transition_to.as_ref());
                        if let Some(next_state) = transition {
                            let current_state = &entry_ref.issue.state;
                            let next_lower = next_state.to_lowercase().replace(' ', "-");
                            let current_lower = current_state.to_lowercase().replace(' ', "-");

                            // Collect routing labels (when_labels) from all matched
                            // stages to remove them during transition. This prevents
                            // backend/frontend labels from persisting into code-review
                            // and causing incorrect re-dispatch during rework.
                            let mut remove = vec![current_lower];
                            for stage in &stages {
                                for label in &stage.when_labels {
                                    let l = label.to_lowercase().replace(' ', "-");
                                    if !remove.contains(&l) {
                                        remove.push(l);
                                    }
                                }
                            }

                            // Clear completed entries for this issue so stages
                            // in the new state can be dispatched fresh.
                            let prefix = format!("{}:", raw_issue_id);
                            self.state.completed.retain(|k| {
                                !k.starts_with(&prefix) && k != raw_issue_id
                            });

                            info!(
                                issue_id = %raw_issue_id,
                                from = ?remove,
                                to = %next_lower,
                                "auto-transitioning issue to next pipeline state"
                            );
                            let tracker = Arc::clone(&self.tracker);
                            let id = raw_issue_id.to_string();
                            let add = vec![next_lower.clone()];
                            tokio::spawn(async move {
                                if let Err(e) = tracker.update_issue_labels(&id, &add, &remove).await {
                                    warn!(error = %e, "failed to auto-transition issue labels");
                                }
                            });
                        }
                    }
                }
            }
            WorkerExitReason::Abnormal(ref err_msg) => {
                let max_retries = 3u32; // Could be made configurable.
                let current_attempt = entry
                    .as_ref()
                    .and_then(|e| e.retry_attempt)
                    .unwrap_or(0);

                if current_attempt < max_retries {
                    let next_attempt = current_attempt + 1;
                    let max_backoff = self.config.agent_max_retry_backoff_ms;
                    let delay_ms = compute_backoff_ms(next_attempt, max_backoff);

                    info!(
                        issue_id = %issue_id,
                        attempt = next_attempt,
                        delay_ms,
                        "scheduling retry"
                    );

                    // Store retry entry.
                    let identifier = entry
                        .map(|e| e.identifier)
                        .unwrap_or_else(|| issue_id.to_string());
                    self.state.retry_attempts.insert(
                        issue_id.to_string(),
                        symphony_core::domain::RetryEntry {
                            issue_id: issue_id.to_string(),
                            identifier,
                            attempt: next_attempt,
                            due_at_ms: delay_ms,
                            error: Some(err_msg.clone()),
                        },
                    );

                    // Spawn a timer to fire the retry event.
                    let retry_tx = self.event_tx.clone();
                    let retry_issue_id = issue_id.to_string();
                    tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        let _ = retry_tx
                            .send(OrchestratorEvent::RetryTimerFired {
                                issue_id: retry_issue_id,
                            })
                            .await;
                    });
                } else {
                    warn!(
                        issue_id = %issue_id,
                        attempt = current_attempt,
                        max_retries,
                        "max retries reached, giving up"
                    );
                    self.state.completed.insert(issue_id.to_string());
                }
            }
        }
    }

    /// Handle a codex update event from a running worker.
    fn handle_codex_update(&mut self, issue_id: &str, event: AgentEvent) {
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        // Build an activity entry for the event.
        let activity = match &event {
            AgentEvent::SessionStarted { session_id, .. } => Some(ActivityEntry {
                event_type: "session_started".to_string(),
                message: format!("Session started: {}", session_id),
                timestamp: now_str.clone(),
            }),
            AgentEvent::TurnCompleted { .. } => Some(ActivityEntry {
                event_type: "turn_completed".to_string(),
                message: "Turn completed".to_string(),
                timestamp: now_str.clone(),
            }),
            AgentEvent::TurnFailed { error, .. } => Some(ActivityEntry {
                event_type: "turn_failed".to_string(),
                message: format!("Turn failed: {}", error),
                timestamp: now_str.clone(),
            }),
            AgentEvent::ApprovalRequested { method, .. } => Some(ActivityEntry {
                event_type: "approval_requested".to_string(),
                message: format!("Approval requested: {}", method),
                timestamp: now_str.clone(),
            }),
            AgentEvent::ApprovalAutoApproved { .. } => Some(ActivityEntry {
                event_type: "auto_approved".to_string(),
                message: "Approval auto-approved".to_string(),
                timestamp: now_str.clone(),
            }),
            AgentEvent::Notification { message, .. } => Some(ActivityEntry {
                event_type: "notification".to_string(),
                message: message.clone(),
                timestamp: now_str.clone(),
            }),
            _ => None,
        };

        if let Some(entry) = activity {
            self.activity_log.push(issue_id, entry);
        }

        if let Some(entry) = self.state.running.get_mut(issue_id) {
            entry.last_codex_timestamp = Some(now);

            match &event {
                AgentEvent::SessionStarted {
                    session_id, pid, ..
                } => {
                    entry.session_id = Some(session_id.clone());
                    entry.codex_app_server_pid = pid.clone();
                    entry.last_codex_event = Some("session_started".to_string());
                }
                AgentEvent::TurnCompleted { usage, .. } => {
                    entry.turn_count += 1;
                    entry.last_codex_event = Some("turn_completed".to_string());
                    if let Some(u) = usage {
                        update_token_counts(entry, u);
                    }
                }
                AgentEvent::TurnFailed { error, .. } => {
                    entry.last_codex_event = Some("turn_failed".to_string());
                    entry.last_codex_message = Some(error.clone());
                }
                AgentEvent::TokenUsageUpdate { usage, .. } => {
                    update_token_counts(entry, usage);
                }
                AgentEvent::RateLimitUpdate { rate_limits, .. } => {
                    self.state.codex_rate_limits = Some(rate_limits.clone());
                }
                AgentEvent::Notification { message, .. } => {
                    entry.last_codex_message = Some(message.clone());
                    entry.last_codex_event = Some("notification".to_string());
                }
                AgentEvent::ApprovalRequested { .. } => {
                    entry.last_codex_event =
                        Some("approval_requested".to_string());
                }
                AgentEvent::ApprovalAutoApproved { .. } => {
                    entry.last_codex_event = Some("auto_approved".to_string());
                }
                _ => {
                    entry.last_codex_event = Some("other".to_string());
                }
            }
        }
    }

    /// Handle a retry timer firing.
    async fn handle_retry_timer(
        &mut self,
        issue_id: &str,
        worker_event_tx: &mpsc::Sender<(String, AgentEvent)>,
    ) {
        let retry_entry = match self.state.retry_attempts.remove(issue_id) {
            Some(entry) => entry,
            None => {
                warn!(
                    issue_id = %issue_id,
                    "retry timer fired but no retry entry found"
                );
                return;
            }
        };

        info!(
            issue_id = %issue_id,
            attempt = retry_entry.attempt,
            "retry timer fired"
        );

        // Fetch current issue state to check eligibility.
        match self
            .tracker
            .fetch_issue_states_by_ids(&[issue_id.to_string()])
            .await
        {
            Ok(issues) => {
                if let Some(issue) = issues.into_iter().next() {
                    if is_dispatch_eligible(&issue, &self.state, &self.config) {
                        self.dispatch_issue(
                            issue,
                            Some(retry_entry.attempt),
                            worker_event_tx,
                        )
                        .await;
                    } else {
                        info!(
                            issue_id = %issue_id,
                            "issue no longer eligible for dispatch on retry"
                        );
                    }
                } else {
                    warn!(issue_id = %issue_id, "issue not found on retry");
                }
            }
            Err(e) => {
                error!(
                    issue_id = %issue_id,
                    error = %e,
                    "failed to fetch issue for retry"
                );
            }
        }
    }

    /// Handle a workflow configuration reload.
    fn handle_workflow_reloaded(
        &mut self,
        config: ServiceConfig,
        prompt_template: String,
    ) {
        info!("workflow reloaded, updating configuration");
        self.state.poll_interval_ms = config.polling_interval_ms;
        self.state.max_concurrent_agents = config.agent_max_concurrent;
        self.config = config;
        self.prompt_template = prompt_template;
    }

    /// Publish the current state to the shared snapshot for the HTTP server.
    fn publish_snapshot(&self) {
        let now = Utc::now();
        let running: Vec<serde_json::Value> = self
            .state
            .running
            .iter()
            .map(|(running_key, e)| {
                // Activity is stored under the running map key (compound for
                // pipeline stages, e.g., "#82:triage"), not the raw issue ID.
                let activity = self.activity_log.get_entries(running_key);
                let activity_json: Vec<serde_json::Value> = activity
                    .into_iter()
                    .map(|a| {
                        serde_json::json!({
                            "event_type": a.event_type,
                            "message": a.message,
                            "timestamp": a.timestamp,
                        })
                    })
                    .collect();

                serde_json::json!({
                    "issue_id": e.issue.id,
                    "issue_identifier": e.identifier,
                    "identifier": e.identifier,
                    "agent_type": e.agent_type,
                    "stage_role": e.stage_role,
                    "state": e.issue.state,
                    "session_id": e.session_id,
                    "turn_count": e.turn_count,
                    "last_codex_event": e.last_codex_event,
                    "last_event": e.last_codex_event,
                    "last_codex_message": e.last_codex_message,
                    "last_message": e.last_codex_message,
                    "last_codex_timestamp": e.last_codex_timestamp.map(|t| t.to_rfc3339()),
                    "last_event_at": e.last_codex_timestamp.map(|t| t.to_rfc3339()),
                    "started_at": e.started_at.to_rfc3339(),
                    "codex_input_tokens": e.codex_input_tokens,
                    "codex_output_tokens": e.codex_output_tokens,
                    "codex_total_tokens": e.codex_total_tokens,
                    "tokens": {
                        "input_tokens": e.codex_input_tokens,
                        "output_tokens": e.codex_output_tokens,
                        "total_tokens": e.codex_total_tokens,
                    },
                    "activity": activity_json,
                })
            })
            .collect();

        let retrying: Vec<serde_json::Value> = self
            .state
            .retry_attempts
            .values()
            .map(|e| {
                serde_json::json!({
                    "issue_id": e.issue_id,
                    "issue_identifier": e.identifier,
                    "identifier": e.identifier,
                    "attempt": e.attempt,
                    "due_at": format!("{}ms", e.due_at_ms),
                    "error": e.error,
                })
            })
            .collect();

        let pending_approvals: Vec<serde_json::Value> = self
            .approval_queue
            .list_pending()
            .into_iter()
            .map(|a| {
                serde_json::json!({
                    "id": a.id,
                    "issue_id": a.issue_id,
                    "issue_identifier": a.issue_identifier,
                    "method": a.method,
                    "created_at": a.created_at,
                })
            })
            .collect();

        let snapshot = serde_json::json!({
            "generated_at": now.to_rfc3339(),
            "counts": {
                "running": running.len(),
                "retrying": retrying.len(),
            },
            "running": running,
            "retrying": retrying,
            "pending_approvals": pending_approvals,
            "codex_totals": {
                "input_tokens": self.state.codex_totals.input_tokens
                    + self.state.running.values().map(|e| e.codex_input_tokens).sum::<u64>(),
                "output_tokens": self.state.codex_totals.output_tokens
                    + self.state.running.values().map(|e| e.codex_output_tokens).sum::<u64>(),
                "total_tokens": self.state.codex_totals.total_tokens
                    + self.state.running.values().map(|e| e.codex_total_tokens).sum::<u64>(),
                // Only report completed sessions' runtime here. The dashboard
                // JS adds running sessions' elapsed time client-side so the
                // counter ticks smoothly between polls.
                "seconds_running": self.state.codex_totals.seconds_running,
            },
            "rate_limits": self.state.codex_rate_limits,
        });

        if let Ok(mut guard) = self.shared_snapshot.write() {
            *guard = snapshot;
        }
    }

    /// Handle graceful shutdown.
    fn handle_shutdown(&self) {
        let running_count = self.state.running.len();
        if running_count > 0 {
            warn!(
                running_count,
                "shutting down with running workers (they will be terminated)"
            );
            for (issue_id, entry) in &self.state.running {
                info!(
                    issue_id = %issue_id,
                    identifier = %entry.identifier,
                    "terminating running worker"
                );
            }
        }
        info!("orchestrator shutdown complete");
    }
}

/// Update token counts on a running entry from a usage report.
fn update_token_counts(
    entry: &mut RunningEntry,
    usage: &symphony_agent::protocol::events::TokenUsage,
) {
    entry.last_reported_input_tokens = entry.codex_input_tokens;
    entry.last_reported_output_tokens = entry.codex_output_tokens;
    entry.last_reported_total_tokens = entry.codex_total_tokens;

    entry.codex_input_tokens = usage.input_tokens;
    entry.codex_output_tokens = usage.output_tokens;
    entry.codex_total_tokens = usage.total_tokens;
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use symphony_core::domain::{AgentProfileConfig, AgentType, HooksConfig, Issue};
    use symphony_core::error::SymphonyError;

    struct NoopTracker;

    #[async_trait]
    impl IssueTracker for NoopTracker {
        async fn fetch_candidate_issues(&self) -> Result<Vec<Issue>, SymphonyError> {
            Ok(vec![])
        }

        async fn fetch_issues_by_states(
            &self,
            _states: &[String],
        ) -> Result<Vec<Issue>, SymphonyError> {
            Ok(vec![])
        }

        async fn fetch_issue_states_by_ids(
            &self,
            _ids: &[String],
        ) -> Result<Vec<Issue>, SymphonyError> {
            Ok(vec![])
        }
    }

    fn test_config() -> ServiceConfig {
        let default_profile = AgentProfileConfig {
            agent_type: AgentType::Codex,
            command: "codex app-server".to_string(),
            approval_policy: None,
            thread_sandbox: None,
            turn_sandbox_policy: None,
            turn_timeout_ms: 3_600_000,
            read_timeout_ms: 5_000,
            stall_timeout_ms: 300_000,
            model: None,
            reasoning_effort: None,
            network_access: true,
            max_turns: None,
        };
        let mut agent_profiles = HashMap::new();
        agent_profiles.insert("codex".to_string(), default_profile);

        ServiceConfig {
            tracker_kind: "github".to_string(),
            tracker_endpoint: "https://api.github.com".to_string(),
            tracker_api_key: "test".to_string(),
            tracker_project_slug: "owner/repo".to_string(),
            tracker_active_states: vec!["Todo".to_string()],
            tracker_terminal_states: vec!["Done".to_string()],
            polling_interval_ms: 30_000,
            workspace_root: PathBuf::from("/tmp/test_workspaces"),
            git_user_name: None,
            git_user_email: None,
            hooks: HooksConfig {
                after_create: None,
                before_run: None,
                after_run: None,
                before_remove: None,
                timeout_ms: 60_000,
            },
            agent_max_concurrent: 3,
            agent_max_turns: 10,
            agent_max_retry_backoff_ms: 300_000,
            agent_max_concurrent_by_state: HashMap::new(),
            agent_require_label: None,
            agent_by_state: HashMap::new(),
            agent_profiles,
            default_agent: "codex".to_string(),
            codex_command: "codex app-server".to_string(),
            codex_approval_policy: None,
            codex_thread_sandbox: None,
            codex_turn_sandbox_policy: None,
            codex_turn_timeout_ms: 3_600_000,
            codex_read_timeout_ms: 5_000,
            codex_stall_timeout_ms: 300_000,
            server_port: None,
            github_app_id: None,
            github_app_installation_id: None,
            github_app_private_key_path: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_network_access: true,
            codex_auto_merge: false,
            pipeline_stages: vec![],
        }
    }

    #[test]
    fn snapshot_empty_state() {
        let config = test_config();
        let tracker: Arc<dyn IssueTracker> = Arc::new(NoopTracker);
        let tmp = tempfile::TempDir::new().unwrap();
        let wm = WorkspaceManager::new(
            tmp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            5000,
        );

        let orch = Orchestrator::new(
            config,
            "prompt".to_string(),
            tracker,
            Arc::new(wm),
        );

        let snap = orch.get_snapshot();
        assert_eq!(snap.running_count, 0);
        assert_eq!(snap.retry_count, 0);
        assert_eq!(snap.completed_count, 0);
    }

    #[test]
    fn event_sender_is_cloneable() {
        let config = test_config();
        let tracker: Arc<dyn IssueTracker> = Arc::new(NoopTracker);
        let tmp = tempfile::TempDir::new().unwrap();
        let wm = WorkspaceManager::new(
            tmp.path().to_path_buf(),
            None,
            None,
            None,
            None,
            5000,
        );

        let orch = Orchestrator::new(
            config,
            "prompt".to_string(),
            tracker,
            Arc::new(wm),
        );

        let _tx1 = orch.event_sender();
        let _tx2 = orch.event_sender();
    }
}
