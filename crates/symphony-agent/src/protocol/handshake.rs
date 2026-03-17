//! Session startup handshake for the Codex app-server protocol.
//!
//! Implements the multi-step initialization sequence:
//! 1. Send `initialize` request, await response
//! 2. Send `initialized` notification
//! 3. Send `thread/start`, extract thread_id
//! 4. Send `turn/start`, extract turn_id
//! 5. Return `SessionInfo` with composite session_id

use std::time::Duration;

use serde_json::Value;
use symphony_core::error::SymphonyError;
use symphony_core::identifiers::compose_session_id;
use tracing::{debug, info};

use crate::process::AgentProcess;
use crate::protocol::messages::{
    build_initialize, build_initialized, build_thread_start, build_turn_start,
};

/// Information about an established agent session.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Thread identifier from the agent.
    pub thread_id: String,
    /// Turn identifier from the agent.
    pub turn_id: String,
    /// Composite session identifier: `<thread_id>-<turn_id>`.
    pub session_id: String,
}

/// Perform the full handshake sequence with the agent process.
///
/// This sends the initialization messages, creates a thread, and starts
/// the first turn. Returns session identifiers on success.
pub async fn perform_handshake(
    process: &mut AgentProcess,
    cwd: &str,
    prompt: &str,
    title: &str,
    approval_policy: &str,
    sandbox: &str,
    sandbox_policy: &str,
    read_timeout: Duration,
) -> Result<SessionInfo, SymphonyError> {
    info!("starting agent handshake");

    // Step 1: Send initialize request.
    let init_req = build_initialize(1);
    let init_json = serde_json::to_string(&init_req).map_err(|e| SymphonyError::ResponseError {
        detail: format!("failed to serialize initialize request: {e}"),
    })?;
    process.write_message(&init_json).await?;
    debug!("sent initialize request");

    // Wait for initialize response.
    let _init_response = read_response_with_timeout(process, read_timeout).await?;
    debug!("received initialize response");

    // Step 2: Send initialized notification.
    let initialized = build_initialized();
    let initialized_json =
        serde_json::to_string(&initialized).map_err(|e| SymphonyError::ResponseError {
            detail: format!("failed to serialize initialized notification: {e}"),
        })?;
    process.write_message(&initialized_json).await?;
    debug!("sent initialized notification");

    // Step 3: Send thread/start request.
    let thread_req = build_thread_start(2, approval_policy, sandbox, cwd);
    let thread_json =
        serde_json::to_string(&thread_req).map_err(|e| SymphonyError::ResponseError {
            detail: format!("failed to serialize thread/start request: {e}"),
        })?;
    process.write_message(&thread_json).await?;
    debug!("sent thread/start request");

    // Read thread_id from response.
    let thread_response = read_response_with_timeout(process, read_timeout).await?;
    let thread_id = extract_string_field(&thread_response, "threadId")
        .or_else(|| extract_string_field(&thread_response, "thread_id"))
        .unwrap_or_else(|| format!("thread-{}", uuid_v4_stub()));
    debug!(thread_id = %thread_id, "received thread_id");

    // Step 4: Send turn/start request.
    let turn_req = build_turn_start(
        3,
        &thread_id,
        prompt,
        cwd,
        title,
        approval_policy,
        sandbox_policy,
    );
    let turn_json =
        serde_json::to_string(&turn_req).map_err(|e| SymphonyError::ResponseError {
            detail: format!("failed to serialize turn/start request: {e}"),
        })?;
    process.write_message(&turn_json).await?;
    debug!("sent turn/start request");

    // Read turn_id from response.
    let turn_response = read_response_with_timeout(process, read_timeout).await?;
    let turn_id = extract_string_field(&turn_response, "turnId")
        .or_else(|| extract_string_field(&turn_response, "turn_id"))
        .unwrap_or_else(|| format!("turn-{}", uuid_v4_stub()));
    debug!(turn_id = %turn_id, "received turn_id");

    let session_id = compose_session_id(&thread_id, &turn_id);
    info!(session_id = %session_id, "handshake completed");

    Ok(SessionInfo {
        thread_id,
        turn_id,
        session_id,
    })
}

/// Read a JSON response from the agent with a timeout.
///
/// Skips empty lines and attempts to parse each line as JSON. Returns
/// the first valid JSON value received. If the timeout expires before
/// a valid response arrives, returns a `ResponseTimeout` error.
async fn read_response_with_timeout(
    process: &mut AgentProcess,
    timeout: Duration,
) -> Result<Value, SymphonyError> {
    let timeout_ms = timeout.as_millis() as u64;

    let result = tokio::time::timeout(timeout, async {
        loop {
            match process.read_line().await? {
                Some(line) if line.is_empty() => continue,
                Some(line) => {
                    match serde_json::from_str::<Value>(&line) {
                        Ok(value) => return Ok(value),
                        Err(e) => {
                            debug!(error = %e, "skipping non-JSON line from agent");
                            continue;
                        }
                    }
                }
                None => {
                    return Err(SymphonyError::PortExit { code: None });
                }
            }
        }
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_elapsed) => Err(SymphonyError::ResponseTimeout { timeout_ms }),
    }
}

/// Extract a string field from a JSON response, searching in both the
/// top-level object and the `result` sub-object.
fn extract_string_field(value: &Value, field: &str) -> Option<String> {
    // Check top-level.
    if let Some(s) = value.get(field).and_then(|v| v.as_str()) {
        return Some(s.to_string());
    }
    // Check inside "result".
    if let Some(s) = value
        .get("result")
        .and_then(|r| r.get(field))
        .and_then(|v| v.as_str())
    {
        return Some(s.to_string());
    }
    None
}

/// Generate a simple stub identifier when the agent does not return one.
///
/// This is not a real UUID but serves as a unique-enough fallback within
/// a single process lifetime.
fn uuid_v4_stub() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{n:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_string_field_top_level() {
        let json = serde_json::json!({"threadId": "abc-123"});
        assert_eq!(
            extract_string_field(&json, "threadId"),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn extract_string_field_in_result() {
        let json = serde_json::json!({"result": {"turnId": "turn-5"}});
        assert_eq!(
            extract_string_field(&json, "turnId"),
            Some("turn-5".to_string())
        );
    }

    #[test]
    fn extract_string_field_missing() {
        let json = serde_json::json!({"other": "value"});
        assert_eq!(extract_string_field(&json, "threadId"), None);
    }

    #[test]
    fn uuid_v4_stub_is_unique() {
        let a = uuid_v4_stub();
        let b = uuid_v4_stub();
        assert_ne!(a, b);
    }
}
