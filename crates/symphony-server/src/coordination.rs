use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::Utc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxMessage {
    pub id: String,
    pub from_role: String,
    pub to_role: String,
    pub body: String,
    pub timestamp: String,
    pub read: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopeClaim {
    pub scope: String,
    pub owner_role: String,
    pub note: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    OwnedByOther { owner_role: String },
}

#[derive(Default)]
struct CoordinationState {
    mailboxes: HashMap<String, HashMap<String, Vec<MailboxMessage>>>,
    claims: HashMap<String, HashMap<String, ScopeClaim>>,
}

pub struct CoordinationStore {
    state: Mutex<CoordinationState>,
    next_message_id: AtomicU64,
    file_io_lock: Mutex<()>,
}

impl CoordinationStore {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(CoordinationState::default()),
            next_message_id: AtomicU64::new(1),
            file_io_lock: Mutex::new(()),
        }
    }

    pub fn send_mailbox(
        &self,
        issue_id: &str,
        from_role: &str,
        to_role: &str,
        body: &str,
    ) -> MailboxMessage {
        let timestamp = Utc::now().to_rfc3339();
        let id = format!(
            "msg-{}-{}",
            Utc::now().timestamp_millis(),
            self.next_message_id.fetch_add(1, Ordering::Relaxed)
        );

        let message = MailboxMessage {
            id,
            from_role: normalize_role(from_role),
            to_role: normalize_role(to_role),
            body: body.trim().to_string(),
            timestamp,
            read: false,
        };

        let mut guard = self.state.lock().expect("coordination store lock poisoned");
        guard
            .mailboxes
            .entry(issue_id.to_string())
            .or_default()
            .entry(message.to_role.clone())
            .or_default()
            .push(message.clone());
        message
    }

    pub fn read_mailbox(
        &self,
        issue_id: &str,
        role: &str,
        include_read: bool,
    ) -> Vec<MailboxMessage> {
        let role = normalize_role(role);
        let guard = self.state.lock().expect("coordination store lock poisoned");
        guard
            .mailboxes
            .get(issue_id)
            .and_then(|by_role| by_role.get(&role))
            .map(|messages| {
                messages
                    .iter()
                    .filter(|message| include_read || !message.read)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn ack_mailbox(&self, issue_id: &str, role: &str, message_id: &str) -> bool {
        let role = normalize_role(role);
        let mut guard = self.state.lock().expect("coordination store lock poisoned");
        let Some(messages) = guard
            .mailboxes
            .get_mut(issue_id)
            .and_then(|by_role| by_role.get_mut(&role))
        else {
            return false;
        };

        if let Some(message) = messages.iter_mut().find(|message| message.id == message_id) {
            message.read = true;
            true
        } else {
            false
        }
    }

    pub fn claim_scope(
        &self,
        issue_id: &str,
        owner_role: &str,
        scope: &str,
        note: &str,
    ) -> Result<ScopeClaim, ClaimError> {
        let owner_role = normalize_role(owner_role);
        let scope_key = normalize_scope_key(scope);
        let mut guard = self.state.lock().expect("coordination store lock poisoned");
        let claims = guard.claims.entry(issue_id.to_string()).or_default();

        if let Some(existing) = claims.get(&scope_key) {
            if existing.owner_role != owner_role {
                return Err(ClaimError::OwnedByOther {
                    owner_role: existing.owner_role.clone(),
                });
            }
        }

        let claim = ScopeClaim {
            scope: scope.trim().to_string(),
            owner_role,
            note: note.trim().to_string(),
            timestamp: Utc::now().to_rfc3339(),
        };
        claims.insert(scope_key, claim.clone());
        Ok(claim)
    }

    pub fn release_scope(
        &self,
        issue_id: &str,
        owner_role: &str,
        scope: &str,
    ) -> Result<bool, ClaimError> {
        let owner_role = normalize_role(owner_role);
        let scope_key = normalize_scope_key(scope);
        let mut guard = self.state.lock().expect("coordination store lock poisoned");
        let Some(claims) = guard.claims.get_mut(issue_id) else {
            return Ok(false);
        };
        let Some(existing) = claims.get(&scope_key) else {
            return Ok(false);
        };
        if existing.owner_role != owner_role {
            return Err(ClaimError::OwnedByOther {
                owner_role: existing.owner_role.clone(),
            });
        }
        claims.remove(&scope_key);
        Ok(true)
    }

    pub fn list_claims(&self, issue_id: &str) -> Vec<ScopeClaim> {
        let guard = self.state.lock().expect("coordination store lock poisoned");
        guard
            .claims
            .get(issue_id)
            .map(|claims| claims.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn append_note_entry(
        &self,
        target_path: &Path,
        message: &str,
    ) -> Result<String, std::io::Error> {
        let _guard = self
            .file_io_lock
            .lock()
            .expect("coordination file lock poisoned");
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(target_path)?;
        writeln!(file, "- [{}] {}", timestamp, message.trim())?;
        Ok(timestamp)
    }

    pub fn append_audit_entry(
        &self,
        audit_path: &Path,
        action: &str,
        role: &str,
        target: Option<&str>,
        detail: Option<&str>,
    ) -> Result<(), std::io::Error> {
        let _guard = self
            .file_io_lock
            .lock()
            .expect("coordination file lock poisoned");
        if let Some(parent) = audit_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(audit_path)?;
        let timestamp = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        writeln!(
            file,
            "{}\t{}\t{}\t{}\t{}",
            timestamp,
            sanitize_audit_field(action),
            sanitize_audit_field(role),
            sanitize_audit_field(target.unwrap_or("")),
            sanitize_audit_field(detail.unwrap_or("")),
        )?;
        Ok(())
    }
}

fn normalize_role(role: &str) -> String {
    role.trim().to_lowercase()
}

fn normalize_scope_key(scope: &str) -> String {
    scope
        .trim()
        .trim_matches('/')
        .to_lowercase()
        .replace('\\', "/")
}

fn sanitize_audit_field(value: &str) -> String {
    value.replace(['\t', '\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailbox_send_read_and_ack_round_trip() {
        let store = CoordinationStore::new();
        let message = store.send_mailbox("42", "Backend-Dev", "Reviewer", "Focus on auth");

        let unread = store.read_mailbox("42", "reviewer", false);
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].id, message.id);
        assert_eq!(unread[0].from_role, "backend-dev");

        assert!(store.ack_mailbox("42", "reviewer", &message.id));
        assert!(store.read_mailbox("42", "reviewer", false).is_empty());
        assert_eq!(store.read_mailbox("42", "reviewer", true).len(), 1);
    }

    #[test]
    fn claim_scope_rejects_other_owner() {
        let store = CoordinationStore::new();
        store
            .claim_scope("42", "backend-dev", "backend/auth", "editing auth")
            .unwrap();

        let result = store.claim_scope("42", "frontend-dev", "backend/auth", "trying to take it");
        assert_eq!(
            result,
            Err(ClaimError::OwnedByOther {
                owner_role: "backend-dev".to_string()
            })
        );
    }
}
