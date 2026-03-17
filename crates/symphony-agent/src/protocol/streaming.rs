//! Turn streaming processor for the Codex app-server protocol.
//!
//! Based on the OpenAI Symphony reference implementation's `receive_loop`
//! and `handle_incoming` functions. Reads JSON-RPC messages from stdout,
//! auto-approves approval requests, handles tool calls, and emits events.

use std::time::Duration;

use chrono::Utc;
use serde_json::Value;
use symphony_core::error::SymphonyError;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::approval::{is_approval_request, is_tool_call, is_user_input_request};
use crate::approval_handler::{ApprovalDecision, ApprovalHandler};
use crate::process::AgentProcess;
use crate::protocol::events::{AgentEvent, TokenUsage};

/// The outcome of a completed turn streaming session.
#[derive(Debug, Clone)]
pub enum TurnResult {
    Completed,
    Failed(String),
    Cancelled,
    TimedOut,
    ProcessExited,
    InputRequired,
}

/// Stream messages from the agent process for a single turn.
pub async fn stream_turn(
    process: &mut AgentProcess,
    event_tx: &mpsc::Sender<AgentEvent>,
    turn_timeout: Duration,
    approval_handler: &dyn ApprovalHandler,
) -> Result<TurnResult, SymphonyError> {
    info!("streaming turn output");

    let result = tokio::time::timeout(turn_timeout, async {
        stream_turn_inner(process, event_tx, approval_handler).await
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_elapsed) => {
            warn!(timeout_ms = turn_timeout.as_millis(), "turn timed out");
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

async fn stream_turn_inner(
    process: &mut AgentProcess,
    event_tx: &mpsc::Sender<AgentEvent>,
    approval_handler: &dyn ApprovalHandler,
) -> Result<TurnResult, SymphonyError> {
    // Accumulator for agent message deltas. We collect all the word-by-word
    // delta fragments and emit a single notification when the message completes.
    let mut pending_message = String::new();

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
                // Non-JSON output from stderr mixed in, or malformed line.
                // Only emit if it looks like it was meant to be a protocol message.
                if line.trim_start().starts_with('{') {
                    let _ = event_tx
                        .send(AgentEvent::Malformed {
                            raw: line,
                            timestamp: Utc::now(),
                        })
                        .await;
                } else {
                    debug!(line = %line.chars().take(200).collect::<String>(), "non-JSON output");
                }
                continue;
            }
        };

        let method = parsed
            .get("method")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let params = parsed.get("params").cloned().unwrap_or(Value::Null);

        // Log every message method for protocol debugging.
        let has_id = parsed.get("id").is_some();
        let has_result = parsed.get("result").is_some();
        let keys: Vec<&str> = parsed.as_object()
            .map(|o| o.keys().map(|k| k.as_str()).collect())
            .unwrap_or_default();
        info!(
            method = %method,
            has_id,
            has_result,
            keys = ?keys,
            "agent message received"
        );

        // Extract usage from any message that carries it (like OpenAI's
        // `metadata_from_message` / `maybe_set_usage`).
        if let Some(usage) = extract_usage_from_message(&parsed) {
            let _ = event_tx
                .send(AgentEvent::TokenUsageUpdate {
                    usage,
                    timestamp: Utc::now(),
                })
                .await;
        }

        // --- Turn lifecycle events (terminal) ---
        match method {
            "turn/completed" | "codex/turnCompleted" => {
                info!("turn completed");
                let usage = extract_token_usage(&params);
                let _ = event_tx
                    .send(AgentEvent::TurnCompleted {
                        timestamp: Utc::now(),
                        usage,
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
                    .send(AgentEvent::TurnCancelled { timestamp: Utc::now() })
                    .await;
                return Ok(TurnResult::Cancelled);
            }

            _ => {}
        }

        // --- Approval requests (delegate to handler) ---
        if is_approval_request(method) {
            let approval_id = extract_request_id(&parsed);
            debug!(method, approval_id = %approval_id, "approval request received");

            let _ = event_tx
                .send(AgentEvent::ApprovalRequested {
                    approval_id: approval_id.clone(),
                    method: method.to_string(),
                    timestamp: Utc::now(),
                })
                .await;

            let decision = approval_handler
                .handle_approval(
                    approval_id.clone(),
                    method.to_string(),
                    params.clone(),
                )
                .await;

            match decision {
                ApprovalDecision::Approve(decision_str) => {
                    debug!(approval_id = %approval_id, decision = %decision_str, "approved");
                    let response = serde_json::json!({
                        "id": parse_id_value(&approval_id),
                        "result": { "decision": decision_str }
                    });
                    let _ = process
                        .write_message(
                            &serde_json::to_string(&response).unwrap_or_default(),
                        )
                        .await;
                    let _ = event_tx
                        .send(AgentEvent::ApprovalAutoApproved {
                            timestamp: Utc::now(),
                        })
                        .await;
                }
                ApprovalDecision::Deny => {
                    debug!(approval_id = %approval_id, "denied");
                    let response = serde_json::json!({
                        "id": parse_id_value(&approval_id),
                        "result": { "decision": "deny" }
                    });
                    let _ = process
                        .write_message(
                            &serde_json::to_string(&response).unwrap_or_default(),
                        )
                        .await;
                }
            }
            continue;
        }

        // --- Tool calls (item/tool/call) ---
        if is_tool_call(method) {
            let request_id = extract_request_id(&parsed);
            let tool_name = params
                .get("tool")
                .or_else(|| params.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            // We don't support any dynamic tools yet - return failure.
            warn!(tool_name = %tool_name, "unsupported tool call");
            let response = serde_json::json!({
                "id": parse_id_value(&request_id),
                "result": {
                    "success": false,
                    "error": "unsupported_tool_call",
                    "output": format!("Tool '{}' is not supported in this Symphony session.", tool_name),
                    "contentItems": [{"type": "inputText", "text": format!("unsupported tool: {}", tool_name)}]
                }
            });
            let _ = process
                .write_message(&serde_json::to_string(&response).unwrap_or_default())
                .await;

            let _ = event_tx
                .send(AgentEvent::UnsupportedToolCall {
                    tool_name,
                    timestamp: Utc::now(),
                })
                .await;
            continue;
        }

        // --- User input required ---
        if is_user_input_request(method, &params) {
            info!("agent requires user input");
            let _ = event_tx
                .send(AgentEvent::TurnInputRequired { timestamp: Utc::now() })
                .await;
            return Ok(TurnResult::InputRequired);
        }

        // --- Token usage updates (thread/tokenUsage/updated) ---
        if method.contains("tokenUsage") || method.contains("token_usage") {
            // The payload is params.tokenUsage.total.{totalTokens, inputTokens, outputTokens}
            if let Some(usage) = extract_token_usage_from_thread_update(&params) {
                let _ = event_tx
                    .send(AgentEvent::TokenUsageUpdate {
                        usage,
                        timestamp: Utc::now(),
                    })
                    .await;
            }
            continue;
        }

        if method.contains("rateLimit") || method.contains("rate_limit") {
            let _ = event_tx
                .send(AgentEvent::RateLimitUpdate {
                    rate_limits: params.clone(),
                    timestamp: Utc::now(),
                })
                .await;
            continue;
        }

        // --- Responses without a method (id + result) ---
        if method.is_empty() && parsed.get("id").is_some() {
            debug!("received response message");
            continue;
        }

        // --- Agent message deltas: accumulate into a single message ---
        if method == "item/agentMessage/delta" {
            if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                pending_message.push_str(delta);
            }
            continue;
        }

        // --- Item completed: flush any pending message first ---
        if method == "item/completed" || method == "item/agentMessage/completed" {
            if !pending_message.is_empty() {
                let text = std::mem::take(&mut pending_message);
                // Truncate very long messages for the activity log.
                let truncated = if text.len() > 500 {
                    format!("{}...", &text[..500])
                } else {
                    text
                };
                let _ = event_tx
                    .send(AgentEvent::Notification {
                        message: truncated,
                        timestamp: Utc::now(),
                    })
                    .await;
            }
            // Also emit the item/completed event itself.
            let item_msg = extract_event_text(method, &params);
            if !item_msg.is_empty() {
                let _ = event_tx
                    .send(AgentEvent::Notification {
                        message: item_msg,
                        timestamp: Utc::now(),
                    })
                    .await;
            }
            continue;
        }

        // --- Item started: flush any prior pending message, then emit ---
        if method == "item/started" || method == "item/agentMessage/started" {
            if !pending_message.is_empty() {
                let text = std::mem::take(&mut pending_message);
                let truncated = if text.len() > 500 {
                    format!("{}...", &text[..500])
                } else {
                    text
                };
                let _ = event_tx
                    .send(AgentEvent::Notification {
                        message: truncated,
                        timestamp: Utc::now(),
                    })
                    .await;
            }
            let item_msg = extract_event_text(method, &params);
            if !item_msg.is_empty() {
                let _ = event_tx
                    .send(AgentEvent::Notification {
                        message: item_msg,
                        timestamp: Utc::now(),
                    })
                    .await;
            }
            continue;
        }

        // --- Other notifications ---
        let message = extract_event_text(method, &params);
        if !message.is_empty() {
            let _ = event_tx
                .send(AgentEvent::Notification {
                    message,
                    timestamp: Utc::now(),
                })
                .await;
        }
    }
}

/// Extract token usage from any message's top-level `usage` field.
///
/// This mirrors OpenAI's `maybe_set_usage` which checks every message
/// payload for a `usage` map.
fn extract_usage_from_message(msg: &Value) -> Option<TokenUsage> {
    let usage = msg.get("usage").or_else(|| {
        msg.get("params").and_then(|p| p.get("usage"))
    })?;

    if !usage.is_object() {
        return None;
    }

    let input = usage
        .get("inputTokens").or_else(|| usage.get("input_tokens"))
        .and_then(|v| v.as_u64()).unwrap_or(0);
    let output = usage
        .get("outputTokens").or_else(|| usage.get("output_tokens"))
        .and_then(|v| v.as_u64()).unwrap_or(0);
    let total = usage
        .get("totalTokens").or_else(|| usage.get("total_tokens"))
        .and_then(|v| v.as_u64()).unwrap_or(input + output);

    if input == 0 && output == 0 && total == 0 {
        return None;
    }

    Some(TokenUsage::new(input, output, total))
}

/// Extract token usage from params (used for turn/completed events).
fn extract_token_usage(params: &Value) -> Option<TokenUsage> {
    let usage = params.get("usage").or_else(|| params.get("tokenUsage"));
    match usage {
        Some(u) if u.is_object() => {
            let input = u.get("inputTokens").or_else(|| u.get("input_tokens"))
                .and_then(|v| v.as_u64()).unwrap_or(0);
            let output = u.get("outputTokens").or_else(|| u.get("output_tokens"))
                .and_then(|v| v.as_u64()).unwrap_or(0);
            let total = u.get("totalTokens").or_else(|| u.get("total_tokens"))
                .and_then(|v| v.as_u64()).unwrap_or(input + output);
            Some(TokenUsage::new(input, output, total))
        }
        _ => {
            // Check for total_token_usage / totalTokenUsage
            let total_usage = params.get("total_token_usage")
                .or_else(|| params.get("totalTokenUsage"));
            if let Some(tu) = total_usage {
                let input = tu.get("input_tokens").or_else(|| tu.get("inputTokens"))
                    .and_then(|v| v.as_u64()).unwrap_or(0);
                let output = tu.get("output_tokens").or_else(|| tu.get("outputTokens"))
                    .and_then(|v| v.as_u64()).unwrap_or(0);
                let total = tu.get("total_tokens").or_else(|| tu.get("totalTokens"))
                    .and_then(|v| v.as_u64()).unwrap_or(input + output);
                Some(TokenUsage::new(input, output, total))
            } else {
                None
            }
        }
    }
}

/// Extract token usage from `thread/tokenUsage/updated` events.
///
/// The payload structure is:
/// `params.tokenUsage.total.{totalTokens, inputTokens, outputTokens}`
fn extract_token_usage_from_thread_update(params: &Value) -> Option<TokenUsage> {
    let token_usage = params.get("tokenUsage")?;
    // Look for the "total" sub-object which has cumulative counts.
    let totals = token_usage.get("total").unwrap_or(token_usage);

    let input = totals
        .get("inputTokens").or_else(|| totals.get("input_tokens"))
        .and_then(|v| v.as_u64()).unwrap_or(0);
    let output = totals
        .get("outputTokens").or_else(|| totals.get("output_tokens"))
        .and_then(|v| v.as_u64()).unwrap_or(0);
    let total = totals
        .get("totalTokens").or_else(|| totals.get("total_tokens"))
        .and_then(|v| v.as_u64()).unwrap_or(input + output);

    if total == 0 && input == 0 && output == 0 {
        return None;
    }

    Some(TokenUsage::new(input, output, total))
}

/// Extract a human-readable message from an agent event for the activity log.
///
/// Handles the main codex event types:
/// - `item/agentMessage/delta` -> partial model output text
/// - `item/started` / `item/completed` -> item type and summary
/// - Other methods -> method name as fallback
fn extract_event_text(method: &str, params: &Value) -> String {
    match method {
        // Agent message deltas contain the actual text the model is producing.
        "item/agentMessage/delta" => {
            params.get("delta")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        }
        // Item started/completed carry the item type.
        "item/started" | "item/completed" => {
            let item_type = params.get("item")
                .and_then(|i| i.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let label = if method.ends_with("started") { "started" } else { "completed" };
            // For command executions, include the command.
            if item_type == "commandExecution" || item_type == "command_execution" {
                let cmd = params.get("item")
                    .and_then(|i| i.get("command"))
                    .and_then(|v| v.as_str())
                    .or_else(|| params.get("item")
                        .and_then(|i| i.get("args"))
                        .and_then(|v| v.as_str()))
                    .unwrap_or("");
                if cmd.is_empty() {
                    format!("{item_type} {label}")
                } else {
                    format!("{item_type} {label}: {}", cmd.chars().take(100).collect::<String>())
                }
            } else {
                format!("{item_type} {label}")
            }
        }
        // Generic fallback.
        _ => {
            params.get("message")
                .and_then(|v| v.as_str())
                .or_else(|| params.get("text").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string()
        }
    }
}

/// Extract the request ID from a JSON-RPC message.
fn extract_request_id(msg: &Value) -> String {
    msg.get("id")
        .map(|v| match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

/// Parse an ID string back to a JSON value (number or string).
fn parse_id_value(id: &str) -> Value {
    if let Ok(n) = id.parse::<u64>() {
        Value::Number(n.into())
    } else {
        Value::String(id.to_string())
    }
}

/// Extract a human-readable message from a notification payload.
fn extract_notification_text(msg: &Value) -> String {
    let params = msg.get("params").unwrap_or(msg);
    params
        .get("message")
        .and_then(|v| v.as_str())
        .or_else(|| params.get("text").and_then(|v| v.as_str()))
        .or_else(|| msg.get("method").and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_token_usage_camel_case() {
        let params = json!({"usage": {"inputTokens": 100, "outputTokens": 200, "totalTokens": 300}});
        let usage = extract_token_usage(&params).unwrap();
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 200);
        assert_eq!(usage.total_tokens, 300);
    }

    #[test]
    fn extract_token_usage_snake_case() {
        let params = json!({"usage": {"input_tokens": 50, "output_tokens": 75, "total_tokens": 125}});
        let usage = extract_token_usage(&params).unwrap();
        assert_eq!(usage.input_tokens, 50);
        assert_eq!(usage.total_tokens, 125);
    }

    #[test]
    fn extract_token_usage_missing() {
        assert!(extract_token_usage(&json!({"other": "data"})).is_none());
    }

    #[test]
    fn extract_usage_from_message_top_level() {
        let msg = json!({"method": "foo", "usage": {"inputTokens": 10, "outputTokens": 5}});
        let usage = extract_usage_from_message(&msg).unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.total_tokens, 15);
    }

    #[test]
    fn extract_usage_from_message_in_params() {
        let msg = json!({"method": "foo", "params": {"usage": {"input_tokens": 20, "output_tokens": 30}}});
        let usage = extract_usage_from_message(&msg).unwrap();
        assert_eq!(usage.input_tokens, 20);
    }

    #[test]
    fn extract_request_id_number() {
        assert_eq!(extract_request_id(&json!({"id": 42})), "42");
    }

    #[test]
    fn extract_request_id_string() {
        assert_eq!(extract_request_id(&json!({"id": "abc"})), "abc");
    }

    #[test]
    fn parse_id_value_number() {
        assert_eq!(parse_id_value("42"), Value::Number(42.into()));
    }

    #[test]
    fn parse_id_value_string() {
        assert_eq!(parse_id_value("abc"), Value::String("abc".into()));
    }

    #[test]
    fn notification_text_from_message() {
        let msg = json!({"method": "some/event", "params": {"message": "Working on tests"}});
        assert_eq!(extract_notification_text(&msg), "Working on tests");
    }

    #[test]
    fn notification_text_fallback_to_method() {
        let msg = json!({"method": "some/event", "params": {}});
        assert_eq!(extract_notification_text(&msg), "some/event");
    }
}
