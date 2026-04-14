//! Worker task that drives the agent through a multi-turn session.
//!
//! Each worker prepares a workspace, runs hooks, starts an agent session,
//! and loops through turns until the issue is resolved, the maximum number
//! of turns is reached, or an error occurs.

use std::path::Path;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use symphony_agent::protocol::events::AgentEvent;
use symphony_agent::protocol::streaming::TurnResult;
use symphony_agent::runner::AgentRunner;
use symphony_core::domain::{AgentType, Issue, ServiceConfig};
use symphony_core::error::SymphonyError;
use symphony_core::identifiers::normalize_state;
use symphony_tracker::traits::IssueTracker;
use symphony_workflow::template::{StageContext, render_prompt, render_prompt_with_stage};
use symphony_workspace::manager::WorkspaceManager;
use tokio::io::AsyncWriteExt;
use tokio::sync::{mpsc, watch};
use tokio::time::{Duration, sleep};
use tracing::{error, info, warn};

use crate::approval_queue::{PendingApprovalQueue, QueuedApprovalHandler};
use crate::events::WorkerExitReason;

// Re-export so the orchestrator can still import from this module.
pub use symphony_agent::runner::AgentRunner as AgentRunnerType;

/// Default continuation prompt for subsequent turns.
const DEFAULT_CONTINUATION_PROMPT: &str =
    "Continue working on the issue. Review your previous changes and verify correctness.";

/// Default Claude CLI internal max-turns guard per Symphony-managed invocation.
const DEFAULT_CLAUDE_CLI_MAX_TURNS: u32 = 20;

/// Result returned by a worker task.
#[derive(Debug)]
pub struct WorkerResult {
    pub issue_id: String,
    pub exit_reason: WorkerExitReason,
}

#[derive(Clone, Debug)]
struct CoordinationPaths {
    readme_file: String,
    shared_file: String,
    handoffs_file: String,
    role_file: String,
    mailboxes_dir: String,
    claims_dir: String,
    events_file: String,
}

impl CoordinationPaths {
    fn for_role(stage_role: Option<&str>) -> Self {
        let role_slug = sanitize_coordination_name(stage_role.unwrap_or("general"));
        Self {
            readme_file: ".symphony/coordination/README.md".to_string(),
            shared_file: ".symphony/coordination/shared.md".to_string(),
            handoffs_file: ".symphony/coordination/handoffs.md".to_string(),
            role_file: format!(".symphony/coordination/roles/{role_slug}.md"),
            mailboxes_dir: ".symphony/coordination/mailboxes".to_string(),
            claims_dir: ".symphony/coordination/claims".to_string(),
            events_file: ".symphony/coordination/events.tsv".to_string(),
        }
    }
}

fn sanitize_coordination_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();

    let sanitized = sanitized.trim_matches('-').trim_matches('_').to_string();
    if sanitized.is_empty() {
        "general".to_string()
    } else {
        sanitized
    }
}

/// Run a complete worker session for a single issue.
///
/// This is the top-level async function spawned as a Tokio task by the
/// orchestrator. It manages the full lifecycle:
///
/// 1. Prepare workspace and run `before_run` via WorkspaceManager
/// 2. Create local coordination files and render the final prompt
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
    mut cancel_rx: tokio::sync::watch::Receiver<bool>,
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

    let stage = config.resolve_stage_for_issue(&issue);
    let stage_roles: Vec<String> = config
        .resolve_stages_for_issue(&issue)
        .into_iter()
        .filter_map(|stage| {
            if let Some(role) = &stage.role {
                Some(role.clone())
            } else if stage.agent != "none" {
                Some(stage.agent.clone())
            } else {
                None
            }
        })
        .collect();

    // Step 1: Prepare workspace and run before_run setup under the
    // per-workspace preparation lock.
    let workspace = match workspace_manager
        .prepare_for_issue(&issue.identifier, Some(&issue_id), Some(&identifier))
        .await
    {
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

    // Step 2: Set git identity in the workspace if configured.
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

    // Step 3: Prepare local coordination files for handoffs and parallel work.
    let coordination =
        match prepare_coordination_files(&workspace_path, stage_role.as_deref()).await {
            Ok(paths) => Some(paths),
            Err(error) => {
                warn!(
                    issue_id = %issue_id,
                    error = %error,
                    "failed to prepare coordination files"
                );
                None
            }
        };

    // Step 4: Render prompt template with issue data and prompt overlays.
    let rendered_prompt = match build_worker_prompt(
        &issue,
        attempt,
        &prompt_template,
        &config,
        stage,
        stage_role.as_deref(),
        scope.as_deref(),
        &stage_roles,
        coordination.as_ref(),
    ) {
        Ok(prompt) => {
            info!(issue_id = %issue_id, prompt_len = prompt.len(), "prompt rendered");
            prompt
        }
        Err(e) => {
            error!(issue_id = %issue_id, error = %e, "failed to render prompt template");
            return WorkerResult {
                issue_id: event_issue_id,
                exit_reason: WorkerExitReason::Abnormal(format!("prompt render failed: {e}")),
            };
        }
    };

    // Create a per-issue event sender that tags events with the issue ID
    // (or compound key for parallel stages).
    let (local_tx, mut local_rx) = mpsc::channel::<AgentEvent>(64);
    let session_env_vars = build_worker_env_vars(&issue, stage_role.as_deref(), scope.as_deref());
    let (coordination_stop_tx, coordination_stop_rx) = watch::channel(false);

    // Forward events with issue ID tagging.
    let forward_tx = event_tx.clone();
    let forward_issue_id = event_issue_id.clone();
    tokio::spawn(async move {
        while let Some(event) = local_rx.recv().await {
            let _ = forward_tx.send((forward_issue_id.clone(), event)).await;
        }
    });

    let coordination_audit_task = spawn_coordination_audit_watcher(
        &workspace_path,
        coordination.as_ref(),
        local_tx.clone(),
        coordination_stop_rx,
    );

    // Branch on agent type.
    let exit_reason = match agent_type {
        AgentType::ClaudeCli => {
            let claude_cli_max_turns = profile_max_turns.unwrap_or(DEFAULT_CLAUDE_CLI_MAX_TURNS);
            run_claude_worker(
                &agent_runner,
                &issue,
                &issue_id,
                &identifier,
                &rendered_prompt,
                max_turns,
                claude_cli_max_turns,
                &workspace_path,
                &session_env_vars,
                &local_tx,
                &mut cancel_rx,
                &config,
                &tracker,
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
                &session_env_vars,
                &local_tx,
                &mut cancel_rx,
                &config,
                &tracker,
                &approval_queue,
            )
            .await
        }
    };

    let _ = coordination_stop_tx.send(true);
    if let Some(handle) = coordination_audit_task {
        let _ = handle.await;
    }

    // Run after_run hook (best effort).
    workspace_manager
        .run_after_run_hook(&workspace_path, Some(&issue_id), Some(&identifier))
        .await;

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

fn build_worker_prompt(
    issue: &Issue,
    attempt: Option<u32>,
    prompt_template: &str,
    config: &ServiceConfig,
    stage: Option<&symphony_core::domain::PipelineStage>,
    stage_role: Option<&str>,
    scope: Option<&str>,
    stage_roles: &[String],
    coordination: Option<&CoordinationPaths>,
) -> Result<String, SymphonyError> {
    let rendered = render_worker_prompt_base(issue, attempt, prompt_template, stage)?;
    let with_state =
        append_state_instructions(rendered, config.state_instruction_for(&issue.state));
    let with_role = append_role_instructions(with_state, config.role_instruction_for(stage_role));
    let with_coordination =
        append_coordination_instructions(with_role, stage_role, scope, stage_roles, coordination);
    Ok(with_coordination)
}

fn build_worker_env_vars(
    issue: &Issue,
    stage_role: Option<&str>,
    scope: Option<&str>,
) -> Vec<(String, String)> {
    let mut vars = vec![
        ("SYMPHONY_ISSUE_ID".to_string(), issue.id.clone()),
        (
            "SYMPHONY_ISSUE_IDENTIFIER".to_string(),
            issue.identifier.clone(),
        ),
        ("SYMPHONY_ISSUE_STATE".to_string(), issue.state.clone()),
    ];

    if let Some(role) = stage_role.filter(|role| !role.trim().is_empty()) {
        vars.push(("SYMPHONY_STAGE_ROLE".to_string(), role.to_string()));
    }
    if let Some(scope) = scope.filter(|scope| !scope.trim().is_empty()) {
        vars.push(("SYMPHONY_STAGE_SCOPE".to_string(), scope.to_string()));
    }

    vars
}

fn parse_coordination_audit_line(line: &str) -> Option<AgentEvent> {
    let mut parts = line.splitn(5, '\t');
    let timestamp = parts.next()?;
    let action = parts.next()?.trim();
    let role = parts.next()?.trim();
    let target = parts.next()?.trim();
    let detail = parts.next()?.trim();

    if action.is_empty() {
        return None;
    }

    let timestamp = DateTime::parse_from_rfc3339(timestamp)
        .ok()?
        .with_timezone(&Utc);

    Some(AgentEvent::CoordinationActivity {
        action: action.to_string(),
        role: (!role.is_empty()).then(|| role.to_string()),
        target: (!target.is_empty()).then(|| target.to_string()),
        detail: (!detail.is_empty()).then(|| detail.to_string()),
        timestamp,
    })
}

fn spawn_coordination_audit_watcher(
    workspace_path: &Path,
    coordination: Option<&CoordinationPaths>,
    event_tx: mpsc::Sender<AgentEvent>,
    mut stop_rx: watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>> {
    let coordination = coordination?.clone();
    let events_path = workspace_path.join(coordination.events_file);

    Some(tokio::spawn(async move {
        let mut processed_len = 0usize;

        loop {
            if *stop_rx.borrow() {
                let _ =
                    drain_coordination_events(&events_path, &event_tx, &mut processed_len).await;
                break;
            }

            let _ = drain_coordination_events(&events_path, &event_tx, &mut processed_len).await;

            tokio::select! {
                _ = stop_rx.changed() => {}
                _ = sleep(Duration::from_millis(250)) => {}
            }
        }
    }))
}

async fn drain_coordination_events(
    events_path: &std::path::PathBuf,
    event_tx: &mpsc::Sender<AgentEvent>,
    processed_len: &mut usize,
) -> Result<(), std::io::Error> {
    let content = match tokio::fs::read_to_string(events_path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    if content.len() < *processed_len {
        *processed_len = 0;
    }

    if content.len() == *processed_len {
        return Ok(());
    }

    let new_content = &content[*processed_len..];
    *processed_len = content.len();

    for line in new_content.lines() {
        if let Some(event) = parse_coordination_audit_line(line) {
            if event_tx.send(event).await.is_err() {
                break;
            }
        }
    }

    Ok(())
}

fn render_worker_prompt_base(
    issue: &Issue,
    attempt: Option<u32>,
    prompt_template: &str,
    stage: Option<&symphony_core::domain::PipelineStage>,
) -> Result<String, SymphonyError> {
    if let Some(stage) = stage {
        if let Some(stage_prompt) = &stage.prompt {
            let default_rendered = render_prompt(prompt_template, issue, attempt)?;
            let ctx = StageContext {
                role: stage.role.clone(),
                transition_to: stage.transition_to.clone(),
                reject_to: stage.reject_to.clone(),
                default_prompt: default_rendered,
            };
            render_prompt_with_stage(stage_prompt, issue, attempt, Some(&ctx))
        } else {
            let ctx = StageContext {
                role: stage.role.clone(),
                transition_to: stage.transition_to.clone(),
                reject_to: stage.reject_to.clone(),
                default_prompt: String::new(),
            };
            render_prompt_with_stage(prompt_template, issue, attempt, Some(&ctx))
        }
    } else {
        render_prompt(prompt_template, issue, attempt)
    }
}

fn append_state_instructions(prompt: String, state_instruction: Option<&String>) -> String {
    match state_instruction {
        Some(instruction) if !instruction.trim().is_empty() => {
            format!("{prompt}\n\n## State-Specific Instructions\n\n{instruction}",)
        }
        _ => prompt,
    }
}

fn append_role_instructions(prompt: String, role_instruction: Option<&String>) -> String {
    match role_instruction {
        Some(instruction) if !instruction.trim().is_empty() => {
            format!("{prompt}\n\n## Role-Specific Instructions\n\n{instruction}",)
        }
        _ => prompt,
    }
}

fn append_coordination_instructions(
    mut prompt: String,
    stage_role: Option<&str>,
    scope: Option<&str>,
    stage_roles: &[String],
    coordination: Option<&CoordinationPaths>,
) -> String {
    let Some(coordination) = coordination else {
        if let Some(scope) = scope {
            prompt.push_str(&format!(
                "\n\n## Parallel Agent Scope\n\n\
Only modify files inside `{scope}` unless expanding beyond that scope is required to unblock the issue and you document why.\n\
Run only targeted verification for this scope.\n\
Expect other agents to push to the same branch. Rebase before pushing and do not replace their work.\n"
            ));
        }
        return prompt;
    };

    let current_role = stage_role.unwrap_or("general");
    let other_roles: Vec<&str> = stage_roles
        .iter()
        .map(String::as_str)
        .filter(|role| *role != current_role)
        .collect();
    let peers = if other_roles.is_empty() {
        "none".to_string()
    } else {
        other_roles.join(", ")
    };

    prompt.push_str(&format!(
        "\n\n## Agent Coordination\n\n\
Symphony provides local coordination files and helper commands for this issue. Other agents only see durable information you write there or code you push; they do not see your internal reasoning or incidental terminal chatter.\n\n\
Read before major edits:\n\
- `{}`\n\
- `{}`\n\
- `{}`\n\
- `{}`\n\n\
Use them this way:\n\
1. Put durable facts, file ownership, and decisions in `shared.md`.\n\
2. If your tool list includes native Symphony coordination tools, prefer them: `symphony_note`, `symphony_mailbox`, and `symphony_claim`.\n\
3. Otherwise use the workspace helpers: `symphony-note <file> \"message\"`, `symphony-mailbox ...`, and `symphony-claim ...`. These are API-backed when Symphony's internal coordination server is available.\n\
4. Use `symphony-mailbox read` at the start, after rebasing, and before stopping to check direct messages for your role.\n\
5. Use `symphony-mailbox send <role> \"message\"` for targeted active-role communication and `symphony-mailbox ack <message-id>` after handling a message.\n\
6. Use `symphony-claim claim <scope> \"reason\"` before taking ownership of a shared cross-cutting path outside your normal lane, and `symphony-claim list` before broad edits.\n\
7. Update only your own role note file. Do not rewrite another role's role file.\n\
8. Keep notes short and factual. Do not paste large logs or duplicate the GitHub workpad.\n\n\
Current role: `{current_role}`\n\
Other active roles for this state: `{peers}`\n",
        coordination.readme_file,
        coordination.shared_file,
        coordination.handoffs_file,
        coordination.role_file,
    ));

    if let Some(scope) = scope {
        prompt.push_str(&format!(
            "\n## Parallel Agent Scope\n\n\
Only modify files inside `{scope}` unless expanding beyond that scope is required to unblock the issue and you document why.\n\
Do not take over another role's file area just because it is nearby.\n\
Run only targeted verification for this scope.\n\
If you must take ownership of a shared path outside this scope, claim it first with `symphony-claim claim <scope> \"reason\"`.\n\
If another agent updated the shared branch, rebase before pushing.\n"
        ));
    }

    prompt
}

async fn prepare_coordination_files(
    workspace_path: &Path,
    stage_role: Option<&str>,
) -> Result<CoordinationPaths, std::io::Error> {
    let paths = CoordinationPaths::for_role(stage_role);
    tokio::fs::create_dir_all(workspace_path.join(".symphony/coordination/roles")).await?;
    tokio::fs::create_dir_all(workspace_path.join(".symphony/coordination/.locks")).await?;
    tokio::fs::create_dir_all(workspace_path.join(&paths.mailboxes_dir)).await?;
    tokio::fs::create_dir_all(workspace_path.join(&paths.claims_dir)).await?;
    tokio::fs::create_dir_all(workspace_path.join(".symphony_bin")).await?;

    ensure_git_exclude_entries(workspace_path).await?;

    write_file_if_missing(
        &workspace_path.join(&paths.readme_file),
        coordination_readme(&paths, stage_role),
    )
    .await?;
    write_file_if_missing(
        &workspace_path.join(&paths.shared_file),
        "# Symphony Shared Coordination\n\n\
Use this file for durable facts that other agents need: owned paths, decisions, blockers, and short verification notes.\n\
Keep entries concise and append-only where practical.\n"
            .to_string(),
    )
    .await?;
    write_file_if_missing(
        &workspace_path.join(&paths.handoffs_file),
        "# Symphony Handoffs\n\n\
Use this file for targeted baton-passes between roles or states.\n\
Format examples:\n\
- To reviewer: focus on migrations and backwards compatibility.\n\
- To rework: fix the failing auth regression test in auth/login.test.ts.\n"
            .to_string(),
    )
    .await?;
    write_file_if_missing(
        &workspace_path.join(&paths.role_file),
        role_file_template(stage_role.unwrap_or("general")),
    )
    .await?;
    write_file_if_missing(&workspace_path.join(&paths.events_file), String::new()).await?;
    write_executable_script(
        &workspace_path.join(".symphony_bin/symphony-note"),
        coordination_note_script(),
    )
    .await?;
    write_executable_script(
        &workspace_path.join(".symphony_bin/symphony-mailbox"),
        coordination_mailbox_script(),
    )
    .await?;
    write_executable_script(
        &workspace_path.join(".symphony_bin/symphony-claim"),
        coordination_claim_script(),
    )
    .await?;

    Ok(paths)
}

fn coordination_readme(paths: &CoordinationPaths, stage_role: Option<&str>) -> String {
    let role_label = stage_role.unwrap_or("general");
    format!(
        "# Symphony Coordination\n\n\
This directory is a local scratchpad for agents working on the same issue.\n\
It is ignored via `.git/info/exclude`, so use it for coordination rather than committed source changes.\n\n\
Files:\n\
- `{}`: durable shared facts and ownership notes.\n\
- `{}`: targeted baton-passes between roles or future attempts.\n\
- `{}`: notes owned by the current role (`{role_label}`).\n\
- `{}`: append-only audit trail written by Symphony coordination helpers.\n\n\
Directories:\n\
- `{}`: fallback local mailbox storage when the API-backed helper path is unavailable.\n\
- `{}`: fallback local claim storage when the API-backed helper path is unavailable.\n\n\
Helpers:\n\
- These helpers are provisioned automatically per workspace in `.symphony_bin`; no manual install is required.\n\
- Codex sessions may also expose native dynamic tools named `symphony_note`, `symphony_mailbox`, and `symphony_claim`; prefer those when available.\n\
- `symphony-note <file> \"message\"`: append safely to shared/handoff files. Uses the internal Symphony coordination API when available.\n\
- `symphony-mailbox read|send|ack`: targeted active-role coordination. Uses the internal Symphony coordination API when available.\n\
- `symphony-claim claim|release|list`: temporary ownership claims. Uses the internal Symphony coordination API when available.\n\n\
Rules:\n\
1. Prefer short factual updates over prose.\n\
2. Prefer native Symphony coordination tools when the agent exposes them; otherwise use the workspace helpers.\n\
3. Use mailbox messages for active-role coordination and `handoffs.md` for durable future-attempt baton passes.\n\
4. Update your own role file, not someone else's.\n\
5. Check `symphony-claim list` before broad edits and claim shared paths before you expand outside your normal lane.\n\
6. Do not duplicate the GitHub workpad or paste large logs.\n",
        paths.shared_file,
        paths.handoffs_file,
        paths.role_file,
        paths.events_file,
        paths.mailboxes_dir,
        paths.claims_dir,
    )
}

fn role_file_template(role: &str) -> String {
    format!(
        "# Symphony Role Notes: {role}\n\n\
Use this file for role-local notes only.\n\
Suggested sections:\n\
- Owned files or scope\n\
- Current plan\n\
- Final status or blocker\n\
\n\
Use `symphony-mailbox send <role> \"message\"` for direct coordination instead of editing another role file.\n"
    )
}

fn coordination_note_script() -> String {
    r#"#!/bin/sh
set -eu

sanitize_field() {
  printf '%s' "$1" | tr '\t\r\n' '   '
}

current_role() {
  printf '%s' "${SYMPHONY_STAGE_ROLE:-general}"
}

coordination_api() {
  [ -n "${SYMPHONY_COORDINATION_API_URL:-}" ] || return 1
  [ -n "${SYMPHONY_ISSUE_ID:-}" ] || return 1
  command -v curl >/dev/null 2>&1 || return 1
  return 0
}

audit_event() {
  action=$(sanitize_field "$1")
  role=$(sanitize_field "$2")
  target=$(sanitize_field "$3")
  detail=$(sanitize_field "$4")
  audit_lock=".symphony/coordination/.locks/audit.lock"
  attempts=0
  while ! mkdir "$audit_lock" 2>/dev/null; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 200 ]; then
      return 0
    fi
    sleep 0.1
  done

  timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  printf '%s\t%s\t%s\t%s\t%s\n' "$timestamp" "$action" "$role" "$target" "$detail" >> .symphony/coordination/events.tsv
  rmdir "$audit_lock" 2>/dev/null || true
}

if [ "$#" -lt 2 ]; then
  echo "usage: symphony-note <coordination-file> <message>" >&2
  exit 64
fi

target="$1"
shift

case "$target" in
  .symphony/coordination/*) ;;
  *)
    echo "target must live under .symphony/coordination/" >&2
    exit 64
    ;;
esac

if coordination_api; then
  if curl --silent --show-error --fail -X POST \
    --data-urlencode "issue_id=${SYMPHONY_ISSUE_ID}" \
    --data-urlencode "role=$(current_role)" \
    --data-urlencode "workspace_dir=$(pwd -P)" \
    --data-urlencode "target=${target}" \
    --data-urlencode "message=$*" \
    "${SYMPHONY_COORDINATION_API_URL}/notes/append"; then
    exit 0
  fi
fi

mkdir -p "$(dirname "$target")" .symphony/coordination/.locks
lock_name=$(printf '%s' "$target" | tr '/ ' '__' | tr -cd 'A-Za-z0-9._-')
lock_dir=".symphony/coordination/.locks/${lock_name}.lock"
attempts=0
while ! mkdir "$lock_dir" 2>/dev/null; do
  attempts=$((attempts + 1))
  if [ "$attempts" -ge 200 ]; then
    echo "failed to acquire coordination lock for $target" >&2
    exit 75
  fi
  sleep 0.1
done

cleanup() {
  rmdir "$lock_dir" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
printf -- '- [%s] %s\n' "$timestamp" "$*" >> "$target"
audit_event "note.append" "$(current_role)" "$target" "$*"
"#
    .to_string()
}

fn coordination_mailbox_script() -> String {
    r#"#!/bin/sh
set -eu

usage() {
  cat >&2 <<'EOF'
usage:
  symphony-mailbox read [role] [--all]
  symphony-mailbox send <role> <message>
  symphony-mailbox ack <message-id> [role]
EOF
  exit 64
}

sanitize_name() {
  sanitized=$(printf '%s' "$1" | tr 'A-Z' 'a-z' | tr '/: ' '----' | tr -cd 'A-Za-z0-9._-')
  sanitized=$(printf '%s' "$sanitized" | sed 's/^[._-]*//; s/[._-]*$//')
  if [ -z "$sanitized" ]; then
    printf 'general'
  else
    printf '%s' "$sanitized"
  fi
}

current_role() {
  printf '%s' "${SYMPHONY_STAGE_ROLE:-general}"
}

sanitize_field() {
  printf '%s' "$1" | tr '\t\r\n' '   '
}

audit_event() {
  action=$(sanitize_field "$1")
  role=$(sanitize_field "$2")
  target=$(sanitize_field "$3")
  detail=$(sanitize_field "$4")
  audit_lock=".symphony/coordination/.locks/audit.lock"
  attempts=0
  while ! mkdir "$audit_lock" 2>/dev/null; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 200 ]; then
      return 0
    fi
    sleep 0.1
  done

  timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  printf '%s\t%s\t%s\t%s\t%s\n' "$timestamp" "$action" "$role" "$target" "$detail" >> .symphony/coordination/events.tsv
  rmdir "$audit_lock" 2>/dev/null || true
}

mailbox_root=".symphony/coordination/mailboxes"

mkdir -p "$mailbox_root" .symphony/coordination/.locks

coordination_api() {
  [ -n "${SYMPHONY_COORDINATION_API_URL:-}" ] || return 1
  [ -n "${SYMPHONY_ISSUE_ID:-}" ] || return 1
  command -v curl >/dev/null 2>&1 || return 1
  return 0
}

cmd="${1:-}"
[ -n "$cmd" ] || usage
shift || true

case "$cmd" in
  send)
    recipient="${1:-}"
    [ -n "$recipient" ] || usage
    shift || true
    [ "$#" -ge 1 ] || usage

    body="$*"
    sender=$(sanitize_name "$(current_role)")
    recipient=$(sanitize_name "$recipient")
    if coordination_api; then
      if curl --silent --show-error --fail -X POST \
        --data-urlencode "issue_id=${SYMPHONY_ISSUE_ID}" \
        --data-urlencode "from_role=${sender}" \
        --data-urlencode "to_role=${recipient}" \
        --data-urlencode "body=${body}" \
        --data-urlencode "workspace_dir=$(pwd -P)" \
        "${SYMPHONY_COORDINATION_API_URL}/mailbox/send"; then
        exit 0
      fi
    fi
    mailbox_dir="$mailbox_root/$recipient"
    mkdir -p "$mailbox_dir/unread" "$mailbox_dir/read"

    msg_path=$(mktemp "$mailbox_dir/unread/msg.XXXXXX")
    msg_id=$(basename "$msg_path")
    timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)

    {
      printf 'from=%s\n' "$sender"
      printf 'to=%s\n' "$recipient"
      printf 'timestamp=%s\n' "$timestamp"
      printf 'body=%s\n' "$body"
    } > "$msg_path"

    audit_event "mailbox.send" "$sender" "$recipient" "$body"
    printf 'sent id=%s to=%s\n' "$msg_id" "$recipient"
    ;;

  read)
    role="$(current_role)"
    show_all=0
    if [ "${1:-}" = "--all" ]; then
      show_all=1
    elif [ "${1:-}" != "" ]; then
      role="$1"
      if [ "${2:-}" = "--all" ]; then
        show_all=1
      fi
    fi

    role=$(sanitize_name "$role")
    if coordination_api; then
      all_flag=false
      if [ "$show_all" -eq 1 ]; then
        all_flag=true
      fi
      if curl --silent --show-error --fail -G \
        --data-urlencode "issue_id=${SYMPHONY_ISSUE_ID}" \
        --data-urlencode "role=${role}" \
        --data-urlencode "all=${all_flag}" \
        "${SYMPHONY_COORDINATION_API_URL}/mailbox/read"; then
        exit 0
      fi
    fi
    unread_dir="$mailbox_root/$role/unread"
    read_dir="$mailbox_root/$role/read"
    mkdir -p "$unread_dir" "$read_dir"

    found=0
    for file in "$unread_dir"/*; do
      [ -f "$file" ] || continue
      msg_id=$(basename "$file")
      from=$(sed -n 's/^from=//p' "$file" | head -n 1)
      timestamp=$(sed -n 's/^timestamp=//p' "$file" | head -n 1)
      body=$(sed -n 's/^body=//p' "$file" | head -n 1)
      printf 'status=unread id=%s from=%s timestamp=%s body=%s\n' "$msg_id" "$from" "$timestamp" "$body"
      found=1
    done

    if [ "$show_all" -eq 1 ]; then
      for file in "$read_dir"/*; do
        [ -f "$file" ] || continue
        msg_id=$(basename "$file")
        from=$(sed -n 's/^from=//p' "$file" | head -n 1)
        timestamp=$(sed -n 's/^timestamp=//p' "$file" | head -n 1)
        body=$(sed -n 's/^body=//p' "$file" | head -n 1)
        printf 'status=read id=%s from=%s timestamp=%s body=%s\n' "$msg_id" "$from" "$timestamp" "$body"
        found=1
      done
    fi

    if [ "$found" -eq 0 ]; then
      printf 'mailbox empty for %s\n' "$role"
    fi
    ;;

  ack)
    msg_id="${1:-}"
    [ -n "$msg_id" ] || usage
    role=$(sanitize_name "${2:-$(current_role)}")
    if coordination_api; then
      if curl --silent --show-error --fail -X POST \
        --data-urlencode "issue_id=${SYMPHONY_ISSUE_ID}" \
        --data-urlencode "role=${role}" \
        --data-urlencode "message_id=${msg_id}" \
        --data-urlencode "workspace_dir=$(pwd -P)" \
        "${SYMPHONY_COORDINATION_API_URL}/mailbox/ack"; then
        exit 0
      fi
    fi
    mailbox_dir="$mailbox_root/$role"
    unread_file="$mailbox_dir/unread/$msg_id"
    read_file="$mailbox_dir/read/$msg_id"
    lock_dir=".symphony/coordination/.locks/mailbox-${role}.lock"

    mkdir -p "$mailbox_dir/unread" "$mailbox_dir/read"

    attempts=0
    while ! mkdir "$lock_dir" 2>/dev/null; do
      attempts=$((attempts + 1))
      if [ "$attempts" -ge 200 ]; then
        echo "failed to acquire mailbox lock for $role" >&2
        exit 75
      fi
      sleep 0.1
    done

    cleanup() {
      rmdir "$lock_dir" 2>/dev/null || true
    }
    trap cleanup EXIT INT TERM

    if [ ! -f "$unread_file" ]; then
      echo "message not found: $msg_id" >&2
      exit 66
    fi

    mv "$unread_file" "$read_file"
    audit_event "mailbox.ack" "$role" "$msg_id" ""
    printf 'acked id=%s role=%s\n' "$msg_id" "$role"
    ;;

  *)
    usage
    ;;
esac
"#
    .to_string()
}

fn coordination_claim_script() -> String {
    r#"#!/bin/sh
set -eu

usage() {
  cat >&2 <<'EOF'
usage:
  symphony-claim claim <scope> [reason]
  symphony-claim release <scope>
  symphony-claim list
EOF
  exit 64
}

sanitize_name() {
  sanitized=$(printf '%s' "$1" | tr 'A-Z' 'a-z' | tr '/: ' '----' | tr -cd 'A-Za-z0-9._-')
  sanitized=$(printf '%s' "$sanitized" | sed 's/^[._-]*//; s/[._-]*$//')
  if [ -z "$sanitized" ]; then
    printf 'general'
  else
    printf '%s' "$sanitized"
  fi
}

current_role() {
  printf '%s' "${SYMPHONY_STAGE_ROLE:-general}"
}

sanitize_field() {
  printf '%s' "$1" | tr '\t\r\n' '   '
}

audit_event() {
  action=$(sanitize_field "$1")
  role=$(sanitize_field "$2")
  target=$(sanitize_field "$3")
  detail=$(sanitize_field "$4")
  audit_lock=".symphony/coordination/.locks/audit.lock"
  attempts=0
  while ! mkdir "$audit_lock" 2>/dev/null; do
    attempts=$((attempts + 1))
    if [ "$attempts" -ge 200 ]; then
      return 0
    fi
    sleep 0.1
  done

  timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  printf '%s\t%s\t%s\t%s\t%s\n' "$timestamp" "$action" "$role" "$target" "$detail" >> .symphony/coordination/events.tsv
  rmdir "$audit_lock" 2>/dev/null || true
}

claims_root=".symphony/coordination/claims"
locks_root=".symphony/coordination/.locks"

mkdir -p "$claims_root" "$locks_root"

coordination_api() {
  [ -n "${SYMPHONY_COORDINATION_API_URL:-}" ] || return 1
  [ -n "${SYMPHONY_ISSUE_ID:-}" ] || return 1
  command -v curl >/dev/null 2>&1 || return 1
  return 0
}

cmd="${1:-}"
[ -n "$cmd" ] || usage
shift || true

case "$cmd" in
  claim)
    scope="${1:-}"
    [ -n "$scope" ] || usage
    shift || true
    note="$*"
    safe_scope=$(sanitize_name "$scope")
    claim_file="$claims_root/${safe_scope}.claim"
    lock_dir="$locks_root/claim-${safe_scope}.lock"
    owner=$(sanitize_name "$(current_role)")
    if coordination_api; then
      if curl --silent --show-error --fail -X POST \
        --data-urlencode "issue_id=${SYMPHONY_ISSUE_ID}" \
        --data-urlencode "owner_role=${owner}" \
        --data-urlencode "scope=${scope}" \
        --data-urlencode "note=${note}" \
        --data-urlencode "workspace_dir=$(pwd -P)" \
        "${SYMPHONY_COORDINATION_API_URL}/claims/claim"; then
        exit 0
      fi
    fi

    attempts=0
    while ! mkdir "$lock_dir" 2>/dev/null; do
      attempts=$((attempts + 1))
      if [ "$attempts" -ge 200 ]; then
        echo "failed to acquire claim lock for $scope" >&2
        exit 75
      fi
      sleep 0.1
    done

    cleanup() {
      rmdir "$lock_dir" 2>/dev/null || true
    }
    trap cleanup EXIT INT TERM

    if [ -f "$claim_file" ]; then
      existing_owner=$(sed -n 's/^owner=//p' "$claim_file" | head -n 1)
      if [ -n "$existing_owner" ] && [ "$existing_owner" != "$owner" ]; then
        existing_scope=$(sed -n 's/^scope=//p' "$claim_file" | head -n 1)
        echo "scope ${existing_scope:-$scope} is already claimed by $existing_owner" >&2
        exit 73
      fi
    fi

    timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    {
      printf 'owner=%s\n' "$owner"
      printf 'scope=%s\n' "$scope"
      printf 'timestamp=%s\n' "$timestamp"
      printf 'note=%s\n' "$note"
    } > "$claim_file"

    audit_event "claim.acquire" "$owner" "$scope" "$note"
    printf 'claimed scope=%s owner=%s\n' "$scope" "$owner"
    ;;

  release)
    scope="${1:-}"
    [ -n "$scope" ] || usage
    safe_scope=$(sanitize_name "$scope")
    claim_file="$claims_root/${safe_scope}.claim"
    lock_dir="$locks_root/claim-${safe_scope}.lock"
    owner=$(sanitize_name "$(current_role)")
    if coordination_api; then
      if curl --silent --show-error --fail -X POST \
        --data-urlencode "issue_id=${SYMPHONY_ISSUE_ID}" \
        --data-urlencode "owner_role=${owner}" \
        --data-urlencode "scope=${scope}" \
        --data-urlencode "workspace_dir=$(pwd -P)" \
        "${SYMPHONY_COORDINATION_API_URL}/claims/release"; then
        exit 0
      fi
    fi

    attempts=0
    while ! mkdir "$lock_dir" 2>/dev/null; do
      attempts=$((attempts + 1))
      if [ "$attempts" -ge 200 ]; then
        echo "failed to acquire claim lock for $scope" >&2
        exit 75
      fi
      sleep 0.1
    done

    cleanup() {
      rmdir "$lock_dir" 2>/dev/null || true
    }
    trap cleanup EXIT INT TERM

    if [ ! -f "$claim_file" ]; then
      printf 'no claim for scope=%s\n' "$scope"
      exit 0
    fi

    existing_owner=$(sed -n 's/^owner=//p' "$claim_file" | head -n 1)
    if [ -n "$existing_owner" ] && [ "$existing_owner" != "$owner" ]; then
      echo "scope $scope is claimed by $existing_owner" >&2
      exit 73
    fi

    rm -f "$claim_file"
    audit_event "claim.release" "$owner" "$scope" ""
    printf 'released scope=%s owner=%s\n' "$scope" "$owner"
    ;;

  list)
    if coordination_api; then
      if curl --silent --show-error --fail -G \
        --data-urlencode "issue_id=${SYMPHONY_ISSUE_ID}" \
        "${SYMPHONY_COORDINATION_API_URL}/claims/list"; then
        exit 0
      fi
    fi
    found=0
    for file in "$claims_root"/*.claim; do
      [ -f "$file" ] || continue
      scope=$(sed -n 's/^scope=//p' "$file" | head -n 1)
      owner=$(sed -n 's/^owner=//p' "$file" | head -n 1)
      timestamp=$(sed -n 's/^timestamp=//p' "$file" | head -n 1)
      note=$(sed -n 's/^note=//p' "$file" | head -n 1)
      printf 'scope=%s owner=%s timestamp=%s note=%s\n' "$scope" "$owner" "$timestamp" "$note"
      found=1
    done

    if [ "$found" -eq 0 ]; then
      echo "no active claims"
    fi
    ;;

  *)
    usage
    ;;
esac
"#
    .to_string()
}

async fn write_file_if_missing(path: &Path, content: String) -> Result<(), std::io::Error> {
    if tokio::fs::try_exists(path).await? {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::write(path, content).await
}

async fn write_executable_script(path: &Path, content: String) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::write(path, content).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await?;
    }

    Ok(())
}

async fn ensure_git_exclude_entries(workspace_path: &Path) -> Result<(), std::io::Error> {
    let exclude_path = workspace_path.join(".git/info/exclude");
    let Some(parent) = exclude_path.parent() else {
        return Ok(());
    };

    if !tokio::fs::try_exists(workspace_path.join(".git")).await? {
        return Ok(());
    }

    tokio::fs::create_dir_all(parent).await?;

    let mut existing = if tokio::fs::try_exists(&exclude_path).await? {
        tokio::fs::read_to_string(&exclude_path).await?
    } else {
        String::new()
    };

    let entries = [".symphony/", ".symphony_prompt", ".symphony_bin/"];
    let missing: Vec<&str> = entries
        .into_iter()
        .filter(|entry| !existing.lines().any(|line| line.trim() == *entry))
        .collect();

    if missing.is_empty() {
        return Ok(());
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .await?;

    if !existing.is_empty() && !existing.ends_with('\n') {
        file.write_all(b"\n").await?;
        existing.push('\n');
    }

    for entry in missing {
        file.write_all(entry.as_bytes()).await?;
        file.write_all(b"\n").await?;
    }

    Ok(())
}

async fn should_continue_after_turn(
    issue_id: &str,
    tracker: &Arc<dyn IssueTracker>,
    config: &ServiceConfig,
) -> bool {
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
                    return false;
                }

                let is_handoff = if config.pipeline_stages.is_empty() {
                    // Legacy hardcoded list.
                    let handoff_states = [
                        "human review",
                        "human-review",
                        "humanreview",
                        "code review",
                        "code-review",
                        "codereview",
                        "merging",
                        "blocked",
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
                    return false;
                }

                true
            } else {
                warn!(issue_id = %issue_id, "issue not found in tracker, stopping");
                false
            }
        }
        Err(e) => {
            warn!(
                issue_id = %issue_id,
                error = %e,
                "failed to check issue state, continuing"
            );
            true
        }
    }
}

/// Run Claude CLI as Symphony-managed outer turns.
///
/// Each outer turn is one Claude CLI invocation. Claude still manages its own
/// model/tool loop within that invocation. Normal Claude success stops the
/// worker; hitting Claude's CLI max-turns guard resumes the same Claude
/// session if Symphony turns remain and the tracker state is still active.
async fn run_claude_worker(
    agent_runner: &AgentRunner,
    issue: &Issue,
    issue_id: &str,
    _identifier: &str,
    prompt: &str,
    outer_max_turns: u32,
    claude_cli_max_turns: u32,
    workspace_path: &std::path::Path,
    session_env_vars: &[(String, String)],
    event_tx: &mpsc::Sender<AgentEvent>,
    cancel_rx: &mut tokio::sync::watch::Receiver<bool>,
    config: &ServiceConfig,
    tracker: &Arc<dyn IssueTracker>,
) -> WorkerExitReason {
    let mut claude_session_id: Option<String> = None;
    let mut turn_count = 0u32;
    let mut exit_reason = WorkerExitReason::Normal;

    for turn_num in 0..outer_max_turns {
        if *cancel_rx.borrow() {
            info!(issue_id = %issue_id, "worker cancelled by orchestrator");
            exit_reason = WorkerExitReason::Abnormal("cancelled by orchestrator".to_string());
            break;
        }

        let is_first_turn = turn_num == 0;
        let turn_prompt = if is_first_turn {
            prompt.to_string()
        } else {
            DEFAULT_CONTINUATION_PROMPT.to_string()
        };

        info!(
            issue_id = %issue_id,
            turn = turn_num + 1,
            max_turns = outer_max_turns,
            claude_cli_max_turns,
            "starting Claude turn"
        );

        let mut session = match agent_runner
            .start_claude_session(
                workspace_path,
                issue,
                &turn_prompt,
                claude_cli_max_turns,
                claude_session_id.as_deref(),
                turn_num + 1,
                session_env_vars,
                event_tx,
            )
            .await
        {
            Ok(session) => session,
            Err(e) => {
                error!(issue_id = %issue_id, error = %e, "failed to start Claude session");
                exit_reason =
                    WorkerExitReason::Abnormal(format!("claude session start failed: {e}"));
                break;
            }
        };

        if claude_session_id.is_none() {
            claude_session_id = Some(session.session_info.session_id.clone());
        }

        let turn_result = match agent_runner
            .run_claude_session(&mut session, event_tx, cancel_rx)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(issue_id = %issue_id, error = %e, "Claude session error");
                let _ = agent_runner.stop_session(&mut session).await;
                exit_reason = WorkerExitReason::Abnormal(format!("claude session error: {e}"));
                break;
            }
        };

        if let Err(e) = agent_runner.stop_session(&mut session).await {
            warn!(issue_id = %issue_id, error = %e, "failed to stop Claude process cleanly");
        }

        turn_count += 1;

        match turn_result {
            TurnResult::Completed => {
                info!(issue_id = %issue_id, turn = turn_count, "Claude turn completed");
                break;
            }
            TurnResult::MaxTurns => {
                info!(
                    issue_id = %issue_id,
                    turn = turn_count,
                    "Claude hit CLI max-turns guard"
                );
            }
            TurnResult::Failed(ref error) => {
                warn!(issue_id = %issue_id, error = %error, "Claude turn failed");
                exit_reason = WorkerExitReason::Abnormal(format!("claude turn failed: {error}"));
                break;
            }
            TurnResult::Cancelled => {
                info!(issue_id = %issue_id, "Claude turn cancelled");
                exit_reason = WorkerExitReason::Abnormal("claude turn cancelled".to_string());
                break;
            }
            TurnResult::TimedOut => {
                warn!(issue_id = %issue_id, "Claude turn timed out");
                exit_reason = WorkerExitReason::Abnormal("claude turn timed out".to_string());
                break;
            }
            TurnResult::ProcessExited => {
                warn!(issue_id = %issue_id, "Claude process exited");
                exit_reason =
                    WorkerExitReason::Abnormal("claude process exited unexpectedly".to_string());
                break;
            }
            TurnResult::InputRequired => {
                warn!(issue_id = %issue_id, "Claude requires user input");
                exit_reason = WorkerExitReason::Abnormal("claude requires user input".to_string());
                break;
            }
        }

        if turn_num + 1 < outer_max_turns
            && !should_continue_after_turn(issue_id, tracker, config).await
        {
            break;
        }
    }

    info!(
        issue_id = %issue_id,
        turns = turn_count,
        exit_reason = ?exit_reason,
        "Claude worker finished"
    );

    exit_reason
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
    session_env_vars: &[(String, String)],
    local_tx: &mpsc::Sender<AgentEvent>,
    cancel_rx: &mut tokio::sync::watch::Receiver<bool>,
    config: &ServiceConfig,
    tracker: &Arc<dyn IssueTracker>,
    approval_queue: &Arc<PendingApprovalQueue>,
) -> WorkerExitReason {
    let mut session = match agent_runner
        .start_session(
            workspace_path,
            issue,
            rendered_prompt,
            session_env_vars,
            local_tx,
        )
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
                cancel_rx,
            )
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!(issue_id = %issue_id, error = %e, "turn execution error");
                exit_reason = WorkerExitReason::Abnormal(format!("turn execution error: {e}"));
                break;
            }
        };

        turn_count += 1;

        match turn_result {
            TurnResult::Completed => {
                info!(issue_id = %issue_id, turn = turn_count, "turn completed");
            }
            TurnResult::MaxTurns => {
                warn!(issue_id = %issue_id, "unexpected max-turns result from Codex stream");
                exit_reason = WorkerExitReason::Abnormal(
                    "unexpected max-turns result from Codex stream".to_string(),
                );
                break;
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
                exit_reason = WorkerExitReason::Abnormal("agent requires user input".to_string());
                break;
            }
        }

        if turn_num + 1 < max_turns && !should_continue_after_turn(issue_id, tracker, config).await
        {
            break;
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
    use tempfile::TempDir;
    use tokio::process::Command;

    #[test]
    fn default_continuation_prompt_is_not_empty() {
        assert!(!DEFAULT_CONTINUATION_PROMPT.is_empty());
    }

    #[test]
    fn sanitize_coordination_name_replaces_reserved_characters() {
        assert_eq!(
            sanitize_coordination_name("frontend:review"),
            "frontend-review"
        );
        assert_eq!(sanitize_coordination_name("  "), "general");
    }

    #[test]
    fn append_coordination_instructions_mentions_files_scope_and_peers() {
        let prompt = append_coordination_instructions(
            "Base prompt".to_string(),
            Some("backend-dev"),
            Some("backend/"),
            &["backend-dev".to_string(), "frontend-dev".to_string()],
            Some(&CoordinationPaths::for_role(Some("backend-dev"))),
        );

        assert!(prompt.contains("## Agent Coordination"));
        assert!(prompt.contains(".symphony/coordination/shared.md"));
        assert!(prompt.contains(".symphony/coordination/handoffs.md"));
        assert!(prompt.contains(".symphony/coordination/roles/backend-dev.md"));
        assert!(prompt.contains("Other active roles for this state: `frontend-dev`"));
        assert!(prompt.contains("Only modify files inside `backend/`"));
        assert!(prompt.contains("symphony-note <file>"));
        assert!(prompt.contains("symphony-mailbox send <role>"));
        assert!(prompt.contains("symphony-claim claim <scope>"));
    }

    #[test]
    fn append_state_instructions_adds_state_section_when_present() {
        let prompt = append_state_instructions(
            "Base prompt".to_string(),
            Some(&"Review only. Do not implement.".to_string()),
        );

        assert!(prompt.contains("## State-Specific Instructions"));
        assert!(prompt.contains("Review only. Do not implement."));
    }

    #[test]
    fn append_role_instructions_adds_role_section_when_present() {
        let prompt = append_role_instructions(
            "Base prompt".to_string(),
            Some(&"Review diffs only. Do not author fixes.".to_string()),
        );

        assert!(prompt.contains("## Role-Specific Instructions"));
        assert!(prompt.contains("Review diffs only. Do not author fixes."));
    }

    #[test]
    fn coordination_note_script_uses_locking() {
        let script = coordination_note_script();
        assert!(script.contains(".symphony/coordination/.locks"));
        assert!(script.contains("failed to acquire coordination lock"));
        assert!(script.contains("usage: symphony-note"));
    }

    #[test]
    fn build_worker_env_vars_includes_stage_context() {
        let issue = Issue {
            id: "42".to_string(),
            identifier: "#42".to_string(),
            title: "Test".to_string(),
            description: None,
            priority: None,
            state: "in-progress".to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        };

        let vars = build_worker_env_vars(&issue, Some("backend-dev"), Some("crates/api"));
        assert!(vars.contains(&("SYMPHONY_ISSUE_ID".to_string(), "42".to_string())));
        assert!(vars.contains(&("SYMPHONY_ISSUE_IDENTIFIER".to_string(), "#42".to_string())));
        assert!(vars.contains(&("SYMPHONY_STAGE_ROLE".to_string(), "backend-dev".to_string())));
        assert!(vars.contains(&("SYMPHONY_STAGE_SCOPE".to_string(), "crates/api".to_string())));
    }

    #[test]
    fn parse_coordination_audit_line_creates_event() {
        let event = parse_coordination_audit_line(
            "2026-04-10T12:00:00Z\tmailbox.send\tbackend-dev\treviewer\tFocus on token refresh",
        )
        .expect("audit line should parse");

        match event {
            AgentEvent::CoordinationActivity {
                action,
                role,
                target,
                detail,
                ..
            } => {
                assert_eq!(action, "mailbox.send");
                assert_eq!(role.as_deref(), Some("backend-dev"));
                assert_eq!(target.as_deref(), Some("reviewer"));
                assert_eq!(detail.as_deref(), Some("Focus on token refresh"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn coordination_helpers_support_mailbox_and_claims() {
        let dir = TempDir::new().unwrap();
        prepare_coordination_files(dir.path(), Some("backend-dev"))
            .await
            .unwrap();

        let mailbox = dir.path().join(".symphony_bin/symphony-mailbox");
        let claim = dir.path().join(".symphony_bin/symphony-claim");

        let send = Command::new(&mailbox)
            .env("SYMPHONY_STAGE_ROLE", "backend-dev")
            .current_dir(dir.path())
            .arg("send")
            .arg("reviewer")
            .arg("focus on token refresh")
            .output()
            .await
            .unwrap();
        assert!(send.status.success());
        let send_stdout = String::from_utf8(send.stdout).unwrap();
        let msg_id = send_stdout
            .split_whitespace()
            .find_map(|part| part.strip_prefix("id="))
            .unwrap()
            .to_string();

        let read = Command::new(&mailbox)
            .env("SYMPHONY_STAGE_ROLE", "reviewer")
            .current_dir(dir.path())
            .arg("read")
            .output()
            .await
            .unwrap();
        assert!(read.status.success());
        let read_stdout = String::from_utf8(read.stdout).unwrap();
        assert!(read_stdout.contains("status=unread"));
        assert!(read_stdout.contains("from=backend-dev"));
        assert!(read_stdout.contains("body=focus on token refresh"));

        let ack = Command::new(&mailbox)
            .env("SYMPHONY_STAGE_ROLE", "reviewer")
            .current_dir(dir.path())
            .arg("ack")
            .arg(&msg_id)
            .output()
            .await
            .unwrap();
        assert!(ack.status.success());

        let claim_ok = Command::new(&claim)
            .env("SYMPHONY_STAGE_ROLE", "backend-dev")
            .current_dir(dir.path())
            .arg("claim")
            .arg("backend/auth")
            .arg("editing token refresh")
            .output()
            .await
            .unwrap();
        assert!(claim_ok.status.success());

        let claim_conflict = Command::new(&claim)
            .env("SYMPHONY_STAGE_ROLE", "frontend-dev")
            .current_dir(dir.path())
            .arg("claim")
            .arg("backend/auth")
            .arg("trying to take same scope")
            .output()
            .await
            .unwrap();
        assert!(!claim_conflict.status.success());
        let conflict_stderr = String::from_utf8(claim_conflict.stderr).unwrap();
        assert!(conflict_stderr.contains("already claimed by backend-dev"));

        let list = Command::new(&claim)
            .env("SYMPHONY_STAGE_ROLE", "backend-dev")
            .current_dir(dir.path())
            .arg("list")
            .output()
            .await
            .unwrap();
        assert!(list.status.success());
        let list_stdout = String::from_utf8(list.stdout).unwrap();
        assert!(list_stdout.contains("scope=backend/auth"));
        assert!(list_stdout.contains("owner=backend-dev"));

        let note = Command::new(dir.path().join(".symphony_bin/symphony-note"))
            .env("SYMPHONY_STAGE_ROLE", "backend-dev")
            .current_dir(dir.path())
            .arg(".symphony/coordination/shared.md")
            .arg("Owned path: backend/auth")
            .output()
            .await
            .unwrap();
        assert!(note.status.success());

        let audit = tokio::fs::read_to_string(dir.path().join(".symphony/coordination/events.tsv"))
            .await
            .unwrap();
        assert!(audit.contains("\tmailbox.send\tbackend-dev\treviewer\tfocus on token refresh"));
        assert!(audit.contains("\tmailbox.ack\treviewer\t"));
        assert!(
            audit.contains("\tclaim.acquire\tbackend-dev\tbackend/auth\tediting token refresh")
        );
        assert!(audit.contains(
            "\tnote.append\tbackend-dev\t.symphony/coordination/shared.md\tOwned path: backend/auth"
        ));
    }
}
