use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use tracing::warn;

use crate::router::AppState;

#[derive(Deserialize)]
pub struct MailboxReadQuery {
    issue_id: String,
    role: String,
    all: Option<bool>,
}

#[derive(Deserialize)]
pub struct MailboxSendForm {
    issue_id: String,
    from_role: String,
    to_role: String,
    body: String,
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
pub struct MailboxAckForm {
    issue_id: String,
    role: String,
    message_id: String,
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
pub struct ClaimForm {
    issue_id: String,
    owner_role: String,
    scope: String,
    note: Option<String>,
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
pub struct ClaimReleaseForm {
    issue_id: String,
    owner_role: String,
    scope: String,
    workspace_dir: Option<String>,
}

#[derive(Deserialize)]
pub struct ClaimListQuery {
    issue_id: String,
}

#[derive(Deserialize)]
pub struct NoteAppendForm {
    issue_id: String,
    role: String,
    workspace_dir: String,
    target: String,
    message: String,
}

pub async fn get_mailbox_read(
    State(state): State<Arc<AppState>>,
    Query(query): Query<MailboxReadQuery>,
) -> (StatusCode, String) {
    let messages =
        state
            .coordination
            .read_mailbox(&query.issue_id, &query.role, query.all.unwrap_or(false));

    if messages.is_empty() {
        return (
            StatusCode::OK,
            format!("mailbox empty for {}", query.role.trim().to_lowercase()),
        );
    }

    let body = messages
        .into_iter()
        .map(|message| {
            format!(
                "status={} id={} from={} timestamp={} body={}",
                if message.read { "read" } else { "unread" },
                message.id,
                message.from_role,
                message.timestamp,
                sanitize_output_field(&message.body),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    (StatusCode::OK, body)
}

pub async fn post_mailbox_send(
    State(state): State<Arc<AppState>>,
    axum::extract::Form(form): axum::extract::Form<MailboxSendForm>,
) -> (StatusCode, String) {
    let message =
        state
            .coordination
            .send_mailbox(&form.issue_id, &form.from_role, &form.to_role, &form.body);

    record_coordination_activity(
        &state,
        &form.issue_id,
        &message.from_role,
        "mailbox.send",
        Some(message.to_role.clone()),
        Some(message.body.clone()),
        form.workspace_dir.as_deref(),
    )
    .await;

    (
        StatusCode::OK,
        format!("sent id={} to={}", message.id, message.to_role),
    )
}

pub async fn post_mailbox_ack(
    State(state): State<Arc<AppState>>,
    axum::extract::Form(form): axum::extract::Form<MailboxAckForm>,
) -> (StatusCode, String) {
    if !state
        .coordination
        .ack_mailbox(&form.issue_id, &form.role, &form.message_id)
    {
        return (
            StatusCode::NOT_FOUND,
            format!("message not found: {}", form.message_id),
        );
    }

    record_coordination_activity(
        &state,
        &form.issue_id,
        &form.role,
        "mailbox.ack",
        Some(form.message_id.clone()),
        None,
        form.workspace_dir.as_deref(),
    )
    .await;

    (
        StatusCode::OK,
        format!(
            "acked id={} role={}",
            form.message_id,
            form.role.trim().to_lowercase()
        ),
    )
}

pub async fn post_claim_scope(
    State(state): State<Arc<AppState>>,
    axum::extract::Form(form): axum::extract::Form<ClaimForm>,
) -> (StatusCode, String) {
    match state.coordination.claim_scope(
        &form.issue_id,
        &form.owner_role,
        &form.scope,
        form.note.as_deref().unwrap_or(""),
    ) {
        Ok(claim) => {
            record_coordination_activity(
                &state,
                &form.issue_id,
                &claim.owner_role,
                "claim.acquire",
                Some(claim.scope.clone()),
                (!claim.note.is_empty()).then_some(claim.note.clone()),
                form.workspace_dir.as_deref(),
            )
            .await;

            (
                StatusCode::OK,
                format!("claimed scope={} owner={}", claim.scope, claim.owner_role),
            )
        }
        Err(crate::coordination::ClaimError::OwnedByOther { owner_role }) => (
            StatusCode::CONFLICT,
            format!("scope {} is already claimed by {}", form.scope, owner_role),
        ),
    }
}

pub async fn post_release_scope(
    State(state): State<Arc<AppState>>,
    axum::extract::Form(form): axum::extract::Form<ClaimReleaseForm>,
) -> (StatusCode, String) {
    match state
        .coordination
        .release_scope(&form.issue_id, &form.owner_role, &form.scope)
    {
        Ok(true) => {
            record_coordination_activity(
                &state,
                &form.issue_id,
                &form.owner_role,
                "claim.release",
                Some(form.scope.clone()),
                None,
                form.workspace_dir.as_deref(),
            )
            .await;

            (
                StatusCode::OK,
                format!(
                    "released scope={} owner={}",
                    form.scope,
                    form.owner_role.trim().to_lowercase()
                ),
            )
        }
        Ok(false) => (StatusCode::OK, format!("no claim for scope={}", form.scope)),
        Err(crate::coordination::ClaimError::OwnedByOther { owner_role }) => (
            StatusCode::CONFLICT,
            format!("scope {} is claimed by {}", form.scope, owner_role),
        ),
    }
}

pub async fn get_claims_list(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ClaimListQuery>,
) -> (StatusCode, String) {
    let claims = state.coordination.list_claims(&query.issue_id);
    if claims.is_empty() {
        return (StatusCode::OK, "no active claims".to_string());
    }

    let body = claims
        .into_iter()
        .map(|claim| {
            format!(
                "scope={} owner={} timestamp={} note={}",
                claim.scope,
                claim.owner_role,
                claim.timestamp,
                sanitize_output_field(&claim.note),
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    (StatusCode::OK, body)
}

pub async fn post_note_append(
    State(state): State<Arc<AppState>>,
    axum::extract::Form(form): axum::extract::Form<NoteAppendForm>,
) -> (StatusCode, String) {
    let (target_path, audit_path) =
        match resolve_coordination_target_paths(&form.workspace_dir, &form.target) {
            Ok(paths) => paths,
            Err(message) => return (StatusCode::BAD_REQUEST, message),
        };

    if let Err(error) = state
        .coordination
        .append_note_entry(&target_path, &form.message)
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to append coordination note: {error}"),
        );
    }

    let role = normalize_role_value(&form.role);
    if let Err(error) = state.coordination.append_audit_entry(
        &audit_path,
        "note.append",
        &role,
        Some(&form.target),
        Some(&form.message),
    ) {
        warn!(
            error = %error,
            workspace_dir = %form.workspace_dir,
            target = %form.target,
            "failed to mirror note append into coordination audit file"
        );
        emit_coordination_event(
            &state,
            &form.issue_id,
            &role,
            "note.append",
            Some(form.target.clone()),
            Some(form.message.clone()),
        )
        .await;
    }

    (
        StatusCode::OK,
        format!("appended note to {}", form.target.trim()),
    )
}

async fn emit_coordination_event(
    state: &Arc<AppState>,
    issue_id: &str,
    role: &str,
    action: &str,
    target: Option<String>,
    detail: Option<String>,
) {
    let role = normalize_role_value(role);
    let route_issue_id = format!("{}:{}", issue_id, role);
    let _ = state
        .orchestrator_tx
        .send(
            symphony_orchestrator::events::OrchestratorEvent::CodexUpdate {
                issue_id: route_issue_id,
                event: symphony_agent::protocol::events::AgentEvent::CoordinationActivity {
                    action: action.to_string(),
                    role: Some(role),
                    target,
                    detail,
                    timestamp: chrono::Utc::now(),
                },
            },
        )
        .await;
}

async fn record_coordination_activity(
    state: &Arc<AppState>,
    issue_id: &str,
    role: &str,
    action: &str,
    target: Option<String>,
    detail: Option<String>,
    workspace_dir: Option<&str>,
) {
    let role = normalize_role_value(role);
    if !mirror_coordination_audit(
        state,
        workspace_dir,
        action,
        &role,
        target.as_deref(),
        detail.as_deref(),
    ) {
        emit_coordination_event(state, issue_id, &role, action, target, detail).await;
    }
}

fn mirror_coordination_audit(
    state: &Arc<AppState>,
    workspace_dir: Option<&str>,
    action: &str,
    role: &str,
    target: Option<&str>,
    detail: Option<&str>,
) -> bool {
    let Some(workspace_dir) = workspace_dir else {
        return false;
    };
    let Ok(audit_path) = resolve_audit_path(workspace_dir) else {
        return false;
    };
    if let Err(error) =
        state
            .coordination
            .append_audit_entry(&audit_path, action, role, target, detail)
    {
        warn!(
            error = %error,
            workspace_dir = %workspace_dir,
            action = %action,
            "failed to mirror coordination audit file entry"
        );
        return false;
    }
    true
}

fn resolve_coordination_target_paths(
    workspace_dir: &str,
    target: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
    let workspace_root = resolve_workspace_dir(workspace_dir)?;
    let target_rel = validate_coordination_target(target)?;
    let target_path = workspace_root.join(&target_rel);
    let coordination_root = workspace_root.join(".symphony/coordination");
    if !target_path.starts_with(&coordination_root) {
        return Err("target must live under .symphony/coordination/".to_string());
    }
    Ok((target_path, coordination_root.join("events.tsv")))
}

fn resolve_audit_path(workspace_dir: &str) -> Result<std::path::PathBuf, String> {
    Ok(resolve_workspace_dir(workspace_dir)?.join(".symphony/coordination/events.tsv"))
}

fn resolve_workspace_dir(workspace_dir: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = workspace_dir.trim();
    if trimmed.is_empty() {
        return Err("workspace_dir is required".to_string());
    }
    let path = std::path::PathBuf::from(trimmed);
    if !path.is_absolute() {
        return Err("workspace_dir must be an absolute path".to_string());
    }
    std::fs::canonicalize(&path).map_err(|error| format!("invalid workspace_dir: {error}"))
}

fn validate_coordination_target(target: &str) -> Result<std::path::PathBuf, String> {
    let trimmed = target.trim();
    if trimmed.is_empty() {
        return Err("target is required".to_string());
    }
    let path = std::path::Path::new(trimmed);
    if path.is_absolute() {
        return Err("target must be a relative coordination path".to_string());
    }
    let mut components = path.components();
    match components.next() {
        Some(std::path::Component::Normal(component))
            if component == std::ffi::OsStr::new(".symphony") => {}
        _ => return Err("target must live under .symphony/coordination/".to_string()),
    }
    match components.next() {
        Some(std::path::Component::Normal(component))
            if component == std::ffi::OsStr::new("coordination") => {}
        _ => return Err("target must live under .symphony/coordination/".to_string()),
    }
    for component in components {
        match component {
            std::path::Component::Normal(_) => {}
            _ => return Err("target must not contain traversal components".to_string()),
        }
    }
    Ok(path.to_path_buf())
}

fn normalize_role_value(role: &str) -> String {
    role.trim().to_lowercase()
}

fn sanitize_output_field(value: &str) -> String {
    value.replace(['\n', '\r', '\t'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::extract::Form;
    use serde_json::json;
    use symphony_agent::protocol::events::AgentEvent;
    use symphony_orchestrator::approval_queue::PendingApprovalQueue;
    use symphony_orchestrator::events::OrchestratorEvent;
    use tempfile::tempdir;

    use crate::coordination::CoordinationStore;

    fn test_app_state() -> (
        Arc<AppState>,
        tokio::sync::mpsc::Receiver<OrchestratorEvent>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let state = Arc::new(AppState {
            orchestrator_tx: tx,
            snapshot_fn: Arc::new(|| json!({ "ok": true })),
            approval_queue: Arc::new(PendingApprovalQueue::new()),
            coordination: Arc::new(CoordinationStore::new()),
        });
        (state, rx)
    }

    #[tokio::test]
    async fn mailbox_send_and_read_emit_coordination_event() {
        let (state, mut rx) = test_app_state();

        let (status, body) = post_mailbox_send(
            State(Arc::clone(&state)),
            Form(MailboxSendForm {
                issue_id: "42".to_string(),
                from_role: "Backend-Dev".to_string(),
                to_role: "Reviewer".to_string(),
                body: "Focus on auth paths".to_string(),
                workspace_dir: None,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("sent id="));
        assert!(body.contains("to=reviewer"));

        let event = rx.recv().await.expect("expected coordination event");
        match event {
            OrchestratorEvent::CodexUpdate { issue_id, event } => {
                assert_eq!(issue_id, "42:backend-dev");
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
                        assert_eq!(detail.as_deref(), Some("Focus on auth paths"));
                    }
                    other => panic!("unexpected agent event: {other:?}"),
                }
            }
            other => panic!("unexpected orchestrator event: {other:?}"),
        }

        let (status, body) = get_mailbox_read(
            State(state),
            Query(MailboxReadQuery {
                issue_id: "42".to_string(),
                role: "reviewer".to_string(),
                all: Some(false),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("status=unread"));
        assert!(body.contains("from=backend-dev"));
        assert!(body.contains("body=Focus on auth paths"));
    }

    #[tokio::test]
    async fn claim_release_emits_events_and_conflicts_for_other_roles() {
        let (state, mut rx) = test_app_state();

        let (status, body) = post_claim_scope(
            State(Arc::clone(&state)),
            Form(ClaimForm {
                issue_id: "77".to_string(),
                owner_role: "Backend-Dev".to_string(),
                scope: "backend/auth".to_string(),
                note: Some("editing auth".to_string()),
                workspace_dir: None,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("claimed scope=backend/auth owner=backend-dev"));

        let acquired = rx.recv().await.expect("expected claim event");
        match acquired {
            OrchestratorEvent::CodexUpdate { issue_id, event } => {
                assert_eq!(issue_id, "77:backend-dev");
                match event {
                    AgentEvent::CoordinationActivity {
                        action,
                        role,
                        target,
                        detail,
                        ..
                    } => {
                        assert_eq!(action, "claim.acquire");
                        assert_eq!(role.as_deref(), Some("backend-dev"));
                        assert_eq!(target.as_deref(), Some("backend/auth"));
                        assert_eq!(detail.as_deref(), Some("editing auth"));
                    }
                    other => panic!("unexpected agent event: {other:?}"),
                }
            }
            other => panic!("unexpected orchestrator event: {other:?}"),
        }

        let (status, body) = post_claim_scope(
            State(Arc::clone(&state)),
            Form(ClaimForm {
                issue_id: "77".to_string(),
                owner_role: "Frontend-Dev".to_string(),
                scope: "backend/auth".to_string(),
                note: Some("trying to take over".to_string()),
                workspace_dir: None,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.contains("already claimed by backend-dev"));

        let (status, body) = post_release_scope(
            State(state),
            Form(ClaimReleaseForm {
                issue_id: "77".to_string(),
                owner_role: "Backend-Dev".to_string(),
                scope: "backend/auth".to_string(),
                workspace_dir: None,
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("released scope=backend/auth owner=backend-dev"));

        let released = rx.recv().await.expect("expected release event");
        match released {
            OrchestratorEvent::CodexUpdate { issue_id, event } => {
                assert_eq!(issue_id, "77:backend-dev");
                match event {
                    AgentEvent::CoordinationActivity {
                        action,
                        role,
                        target,
                        ..
                    } => {
                        assert_eq!(action, "claim.release");
                        assert_eq!(role.as_deref(), Some("backend-dev"));
                        assert_eq!(target.as_deref(), Some("backend/auth"));
                    }
                    other => panic!("unexpected agent event: {other:?}"),
                }
            }
            other => panic!("unexpected orchestrator event: {other:?}"),
        }
    }

    #[tokio::test]
    async fn note_append_writes_file_and_audit_without_direct_event() {
        let (state, mut rx) = test_app_state();
        let dir = tempdir().expect("temp dir");
        let coordination_dir = dir.path().join(".symphony/coordination");
        std::fs::create_dir_all(&coordination_dir).expect("coordination dir");

        let (status, body) = post_note_append(
            State(state),
            Form(NoteAppendForm {
                issue_id: "88".to_string(),
                role: "Reviewer".to_string(),
                workspace_dir: dir.path().display().to_string(),
                target: ".symphony/coordination/shared.md".to_string(),
                message: "Owned paths: backend/auth".to_string(),
            }),
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("appended note"));

        let shared =
            std::fs::read_to_string(coordination_dir.join("shared.md")).expect("shared contents");
        assert!(shared.contains("Owned paths: backend/auth"));

        let audit =
            std::fs::read_to_string(coordination_dir.join("events.tsv")).expect("audit contents");
        assert!(audit.contains("\tnote.append\treviewer\t.symphony/coordination/shared.md\t"));
        assert!(audit.contains("Owned paths: backend/auth"));

        assert!(
            rx.try_recv().is_err(),
            "note append should rely on audit watcher path"
        );
    }
}
