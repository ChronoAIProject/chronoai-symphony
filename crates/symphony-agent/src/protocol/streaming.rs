//! Turn streaming processor for the Codex app-server protocol.
//!
//! Reads JSON-RPC messages from the agent process during a turn, dispatching
//! events to the orchestrator and handling approval requests, tool-call
//! failures, and turn lifecycle messages.

use std::time::Duration;

use chrono::Utc;
use serde_json::Value;
use symphony_core::error::SymphonyError;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::approval::{is_user_input_request, should_auto_approve};
use crate::process::AgentProcess;
use crate::protocol::events::{AgentEvent, TokenUsage};
use crate::protocol::messages::{build_approval_response, build_tool_call_failure};

/// The outcome of a completed turn streaming session.
#[derive(Debug, Clone)]
pub enum TurnResult {
    /// Turn completed successfully.
    Completed,
    /// Turn ended with an error.
    Failed(String),
    /// Turn was cancelled.
    Cancelled,
    /// Turn exceeded the time limit.
    TimedOut,
    /// Agent process exited unexpectedly.
    ProcessExited,
    /// Agent requires user input to continue.
    InputRequired,
}

/// Stream messages from the agent process for a single turn.
///
/// Reads JSON-RPC messages from stdout, handles approval requests
/// automatically, forwards events to the orchestrator via `event_tx`,
/// and returns when the turn reaches a terminal state or times out.
pub async fn stream_turn(
    process: &mut AgentProcess,
    event_tx: &mpsc::Sender<AgentEvent>,
    turn_timeout: Duration,
) -> Result<TurnResult, SymphonyError> {
    info!("streaming turn output");

    let result = tokio::time::timeout(turn_timeout, async {
        stream_turn_inner(process, event_tx).await
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_elapsed) => {
            warn!(
                timeout_ms = turn_timeout.as_millis(),
                "turn timed out"
            );
            let _ = event_tx
                .send(AgentEvent::TurnFailed {
                    error: "turn timed out".to_string(),
                    timestamp: Utc::now(),
                })
                .await;
            Ok(TurnResult::TimedOut)
        }
    }
}

/// Inner streaming loop without the timeout wrapper.
async fn stream_turn_inner(
    process: &mut AgentProcess,
    event_tx: &mpsc::Sender<AgentEvent>,
) -> Result<TurnResult, SymphonyError> {
    loop {
        let line = match process.read_line().await? {
            Some(line) if line.is_empty() => continue,
            Some(line) => line,
            None => {
                info!("agent process exited (EOF)");
                return Ok(TurnResult::ProcessExited);
            }
        };

        let parsed: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                let _ = event_tx
                    .send(AgentEvent::Malformed {
                        raw: line,
                        timestamp: Utc::now(),
                    })
                    .await;
                continue;
            }
        };

        let method = parsed
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let params = parsed
            .get("params")
            .cloned()
            .unwrap_or(Value::Null);

        // Handle turn lifecycle events.
        match method {
            "turn/completed" | "codex/turnCompleted" => {
                info!("turn completed");
                let usage = extract_token_usage(&params);
                let _ = event_tx
                    .send(AgentEvent::TurnCompleted {
                        timestamp: Utc::now(),
                        usage: usage.clone(),
                    })
                    .await;
                return Ok(TurnResult::Completed);
            }

            "turn/failed" | "codex/turnFailed" => {
                let error = params
                    .get("error")
                    .and_then(|v| v.as_str())
                    .or_else(|| params.get("message").and_then(|v| v.as_str()))
                    .unwrap_or("unknown error")
                    .to_string();
                warn!(error = %error, "turn failed");
                let _ = event_tx
                    .send(AgentEvent::TurnFailed {
                        error: error.clone(),
                        timestamp: Utc::now(),
                    })
                    .await;
                return Ok(TurnResult::Failed(error));
            }

            "turn/cancelled" | "codex/turnCancelled" => {
                info!("turn cancelled");
                let _ = event_tx
                    .send(AgentEvent::TurnCancelled {
                        timestamp: Utc::now(),
                    })
                    .await;
                return Ok(TurnResult::Cancelled);
            }

            _ => {}
        }

        // Check for user input required.
        if is_user_input_request(method, &params) {
            info!("agent requires user input");
            let _ = event_tx
                .send(AgentEvent::TurnInputRequired {
                    timestamp: Utc::now(),
                })
                .await;
            return Ok(TurnResult::InputRequired);
        }

        // Handle approval requests.
        if should_auto_approve(method) {
            let request_id = parsed
                .get("id")
                .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| {
                    v.as_u64().map(|n| n.to_string())
                }))
                .unwrap_or_default();

            debug!(method, request_id = %request_id, "auto-approving request");
            let response = build_approval_response(&request_id, true);
            let response_json = serde_json::to_string(&response)
                .unwrap_or_default();
            if let Err(e) = process.write_message(&response_json).await {
                warn!(error = %e, "failed to send approval response");
            }

            let _ = event_tx
                .send(AgentEvent::ApprovalAutoApproved {
                    timestamp: Utc::now(),
                })
                .await;
            continue;
        }

        // Handle token usage updates.
        if method.contains("tokenUsage") || method.contains("token_usage") {
            if let Some(usage) = extract_token_usage(&params) {
                let _ = event_tx
                    .send(AgentEvent::TokenUsageUpdate {
                        usage,
                        timestamp: Utc::now(),
                    })
                    .await;
            }
            continue;
        }

        // Handle rate limit updates.
        if method.contains("rateLimit") || method.contains("rate_limit") {
            let _ = event_tx
                .send(AgentEvent::RateLimitUpdate {
                    rate_limits: params.clone(),
                    timestamp: Utc::now(),
                })
                .await;
            continue;
        }

        // Handle notification messages.
        if method.starts_with("notifications/") || method.starts_with("codex/notification") {
            let message = params
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let _ = event_tx
                .send(AgentEvent::Notification {
                    message,
                    timestamp: Utc::now(),
                })
                .await;
            continue;
        }

        // Handle unsupported tool calls (methods requesting execution we
        // do not recognize).
        if method.contains("toolCall") || method.contains("tool_call") {
            let tool_name = params
                .get("name")
                .or_else(|| params.get("tool"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            warn!(tool_name = %tool_name, "unsupported tool call");

            let request_id = parsed
                .get("id")
                .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| {
                    v.as_u64().map(|n| n.to_string())
                }))
                .unwrap_or_default();

            let failure = build_tool_call_failure(
                &request_id,
                &format!("unsupported tool: {tool_name}"),
            );
            let failure_json = serde_json::to_string(&failure).unwrap_or_default();
            if let Err(e) = process.write_message(&failure_json).await {
                warn!(error = %e, "failed to send tool call failure response");
            }

            let _ = event_tx
                .send(AgentEvent::UnsupportedToolCall {
                    tool_name,
                    timestamp: Utc::now(),
                })
                .await;
            continue;
        }

        // Fallthrough: emit as OtherMessage.
        let _ = event_tx
            .send(AgentEvent::OtherMessage {
                raw: parsed,
                timestamp: Utc::now(),
            })
            .await;
    }
}

/// Extract token usage information from a JSON params object.
fn extract_token_usage(params: &Value) -> Option<TokenUsage> {
    let usage = params.get("usage").or_else(|| params.get("tokenUsage"));

    match usage {
        Some(u) => {
            let input = u
                .get("inputTokens")
                .or_else(|| u.get("input_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let output = u
                .get("outputTokens")
                .or_else(|| u.get("output_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let total = u
                .get("totalTokens")
                .or_else(|| u.get("total_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(input + output);

            Some(TokenUsage::new(input, output, total))
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_token_usage_camel_case() {
        let params = json!({
            "usage": {
                "inputTokens": 100,
                "outputTokens": 200,
                "totalTokens": 300
            }
        });
        let usage = extract_token_usage(&params).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.total_tokens, 300);
    }

    #[test]
    fn extract_token_usage_snake_case() {
        let params = json!({
            "usage": {
                "input_tokens": 50,
                "output_tokens": 75,
                "total_tokens": 125
            }
        });
        let usage = extract_token_usage(&params).unwrap();
        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.output_tokens, 75);
        assert_eq!(usage.total_tokens, 125);
    }

    #[test]
    fn extract_token_usage_missing() {
        let params = json!({"other": "data"});
        assert!(extract_token_usage(&params).is_none());
    }

    #[test]
    fn extract_token_usage_calculates_total() {
        let params = json!({
            "usage": {
                "inputTokens": 10,
                "outputTokens": 20
            }
        });
        let usage = extract_token_usage(&params).unwrap();
        assert_eq!(usage.total_tokens, 30);
    }
}
