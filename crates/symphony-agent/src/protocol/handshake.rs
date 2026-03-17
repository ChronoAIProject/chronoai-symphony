//! Session startup handshake for the Codex app-server protocol.
//!
//! Based on the OpenAI Symphony reference implementation's exact
//! handshake sequence: initialize -> initialized -> thread/start -> turn/start.

use std::time::Duration;

use serde_json::Value;
use symphony_core::error::SymphonyError;
use symphony_core::identifiers::compose_session_id;
use tracing::{debug, error, info};

use crate::process::AgentProcess;
use crate::protocol::messages::{
    build_initialize, build_initialized, build_thread_start, build_turn_start,
    default_approval_policy, default_thread_sandbox, default_turn_sandbox_policy,
};

/// Information about an established agent session.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub thread_id: String,
    pub turn_id: String,
    pub session_id: String,
}

/// Perform the full handshake sequence with the agent process.
///
/// Sends initialization messages, creates a thread, and starts the first
/// turn. Uses the OpenAI-compatible approval policy and sandbox defaults
/// unless overridden in config.
pub async fn perform_handshake(
    process: &mut AgentProcess,
    cwd: &str,
    prompt: &str,
    title: &str,
    approval_policy: Option<&Value>,
    sandbox: Option<&Value>,
    sandbox_policy: Option<&Value>,
    read_timeout: Duration,
) -> Result<SessionInfo, SymphonyError> {
    info!("starting agent handshake");

    let default_ap = default_approval_policy();
    let default_sb = default_thread_sandbox();
    let default_sp = default_turn_sandbox_policy(cwd);

    let ap = approval_policy.unwrap_or(&default_ap);
    let sb = sandbox.unwrap_or(&default_sb);
    let sp = sandbox_policy.unwrap_or(&default_sp);

    // Step 1: Send initialize request.
    let init_req = build_initialize(1);
    send_json(process, &init_req).await?;
    debug!("sent initialize request");

    // Wait for initialize response (id=1).
    let init_response = read_response_with_timeout(process, read_timeout, 1).await?;
    check_response_error(&init_response, "initialize")?;
    debug!("received initialize response");

    // Step 2: Send initialized notification.
    let initialized = build_initialized();
    send_json(process, &initialized).await?;
    debug!("sent initialized notification");

    // Step 3: Send thread/start request.
    let thread_req = build_thread_start(2, ap, sb, cwd);
    send_json(process, &thread_req).await?;
    debug!("sent thread/start request");

    // Read thread_id from response (id=2).
    let thread_response = read_response_with_timeout(process, read_timeout, 2).await?;
    check_response_error(&thread_response, "thread/start")?;

    // Extract thread_id from result.thread.id (OpenAI format).
    let thread_id = thread_response
        .get("result")
        .and_then(|r| r.get("thread"))
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            error!(response = %thread_response, "missing thread.id in thread/start response");
            SymphonyError::ResponseError {
                detail: format!("missing thread.id in thread/start response: {thread_response}"),
            }
        })?;
    debug!(thread_id = %thread_id, "received thread_id");

    // Step 4: Send turn/start request.
    let turn_req = build_turn_start(3, &thread_id, prompt, cwd, title, ap, sp);
    send_json(process, &turn_req).await?;
    debug!("sent turn/start request");

    // Read turn_id from response (id=3).
    let turn_response = read_response_with_timeout(process, read_timeout, 3).await?;
    check_response_error(&turn_response, "turn/start")?;

    // Extract turn_id from result.turn.id (OpenAI format).
    let turn_id = turn_response
        .get("result")
        .and_then(|r| r.get("turn"))
        .and_then(|t| t.get("id"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            error!(response = %turn_response, "missing turn.id in turn/start response");
            SymphonyError::ResponseError {
                detail: format!("missing turn.id in turn/start response: {turn_response}"),
            }
        })?;
    debug!(turn_id = %turn_id, "received turn_id");

    let session_id = compose_session_id(&thread_id, &turn_id);
    info!(session_id = %session_id, "handshake completed");

    Ok(SessionInfo {
        thread_id,
        turn_id,
        session_id,
    })
}

/// Check a response for JSON-RPC errors and fail with a descriptive message.
fn check_response_error(response: &Value, step: &str) -> Result<(), SymphonyError> {
    if let Some(err) = response.get("error") {
        let message = err
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(0);
        error!(step, code, message, "agent handshake error");
        return Err(SymphonyError::ResponseError {
            detail: format!("{step} failed: [{code}] {message}"),
        });
    }
    Ok(())
}

/// Serialize and send a JSON message to the agent process.
async fn send_json<T: serde::Serialize>(
    process: &mut AgentProcess,
    msg: &T,
) -> Result<(), SymphonyError> {
    let json = serde_json::to_string(msg).map_err(|e| SymphonyError::ResponseError {
        detail: format!("failed to serialize message: {e}"),
    })?;
    process.write_message(&json).await
}

/// Read a JSON response matching a specific request ID from the agent.
///
/// Skips messages that do not match the expected ID, just like OpenAI's
/// `await_response` which ignores unrelated messages.
async fn read_response_with_timeout(
    process: &mut AgentProcess,
    timeout: Duration,
    expected_id: u64,
) -> Result<Value, SymphonyError> {
    let timeout_ms = timeout.as_millis() as u64;

    let result = tokio::time::timeout(timeout, async {
        loop {
            match process.read_line().await? {
                Some(line) if line.is_empty() => continue,
                Some(line) => match serde_json::from_str::<Value>(&line) {
                    Ok(value) => {
                        let msg_id = value.get("id").and_then(|v| v.as_u64());
                        if msg_id == Some(expected_id) {
                            return Ok(value);
                        }
                        debug!(
                            expected_id,
                            got_id = ?msg_id,
                            method = value.get("method").and_then(|v| v.as_str()).unwrap_or(""),
                            "skipping non-matching message during handshake"
                        );
                        continue;
                    }
                    Err(_) => {
                        debug!("skipping non-JSON line during handshake");
                        continue;
                    }
                },
                None => return Err(SymphonyError::PortExit { code: None }),
            }
        }
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_elapsed) => Err(SymphonyError::ResponseTimeout { timeout_ms }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_response_error_ok() {
        let resp = serde_json::json!({"id": 1, "result": {}});
        assert!(check_response_error(&resp, "test").is_ok());
    }

    #[test]
    fn check_response_error_detects_error() {
        let resp = serde_json::json!({
            "id": 1,
            "error": {"code": -32600, "message": "bad request"}
        });
        let err = check_response_error(&resp, "test").unwrap_err();
        assert!(err.to_string().contains("bad request"));
    }
}
