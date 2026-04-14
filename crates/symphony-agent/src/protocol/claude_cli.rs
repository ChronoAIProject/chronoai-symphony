//! Native Claude Code CLI adapter.
//!
//! Parses streaming JSON output from `claude -p --output-format stream-json`.
//! Unlike the Codex JSON-RPC protocol, there is no handshake. This adapter
//! streams one Claude CLI invocation; the orchestrator owns the outer turn loop.

use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use symphony_core::error::SymphonyError;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::process::AgentProcess;
use crate::protocol::events::{AgentEvent, TokenUsage};
use crate::protocol::streaming::TurnResult;

const MAX_NOTIFICATION_CHARS: usize = 500;

/// Token usage structure from Claude CLI output.
#[derive(Debug, Deserialize)]
struct ClaudeUsage {
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_creation_input_tokens: Option<u64>,
    cache_read_input_tokens: Option<u64>,
}

impl ClaudeUsage {
    fn to_token_usage(&self) -> TokenUsage {
        let input = self.input_tokens.unwrap_or(0)
            + self.cache_creation_input_tokens.unwrap_or(0)
            + self.cache_read_input_tokens.unwrap_or(0);
        let output = self.output_tokens.unwrap_or(0);
        TokenUsage::new(input, output, input + output)
    }
}

fn truncate_notification(message: impl Into<String>) -> String {
    let message = message.into();

    match message.char_indices().nth(MAX_NOTIFICATION_CHARS) {
        Some((idx, _)) => format!("{}...", &message[..idx]),
        None => message,
    }
}

async fn emit_notification(event_tx: &mpsc::Sender<AgentEvent>, message: impl Into<String>) {
    let _ = event_tx
        .send(AgentEvent::Notification {
            message: truncate_notification(message),
            timestamp: Utc::now(),
        })
        .await;
}

fn non_empty_str(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn summarize_tool_input(input: Option<&Value>) -> Option<String> {
    let input = input?;
    non_empty_str(input.get("command"))
        .map(|s| s.chars().take(120).collect::<String>())
        .or_else(|| non_empty_str(input.get("file_path")).map(|s| s.to_string()))
        .or_else(|| non_empty_str(input.get("pattern")).map(|s| s.to_string()))
}

fn summarize_system_event(parsed: &Value) -> Option<String> {
    let subtype = parsed.get("subtype").and_then(|v| v.as_str())?;
    match subtype {
        "init" => {
            let model = non_empty_str(parsed.get("model"));
            let cwd = non_empty_str(parsed.get("cwd"));
            match (model, cwd) {
                (Some(model), Some(cwd)) => Some(format!("[session:init] model={model} cwd={cwd}")),
                (Some(model), None) => Some(format!("[session:init] model={model}")),
                (None, Some(cwd)) => Some(format!("[session:init] cwd={cwd}")),
                (None, None) => Some("[session:init] Claude Code ready".to_string()),
            }
        }
        "status" => {
            let permission_mode = non_empty_str(parsed.get("permissionMode"));
            let status = non_empty_str(parsed.get("status"));
            match (permission_mode, status) {
                (Some(mode), Some(status)) => {
                    Some(format!("[session:status] {status} (permission={mode})"))
                }
                (Some(mode), None) => Some(format!("[session:status] permission={mode}")),
                (None, Some(status)) => Some(format!("[session:status] {status}")),
                (None, None) => None,
            }
        }
        "hook_started" => {
            let hook_name = non_empty_str(parsed.get("hook_name")).unwrap_or("hook");
            let hook_event = non_empty_str(parsed.get("hook_event"));
            Some(match hook_event {
                Some(event) => format!("[hook:{hook_name}] started ({event})"),
                None => format!("[hook:{hook_name}] started"),
            })
        }
        "hook_progress" => {
            let hook_name = non_empty_str(parsed.get("hook_name")).unwrap_or("hook");
            let detail = non_empty_str(parsed.get("output"))
                .or_else(|| non_empty_str(parsed.get("stdout")))
                .or_else(|| non_empty_str(parsed.get("stderr")));
            Some(match detail {
                Some(detail) => format!("[hook:{hook_name}] {detail}"),
                None => format!("[hook:{hook_name}] running"),
            })
        }
        "hook_response" => {
            let hook_name = non_empty_str(parsed.get("hook_name")).unwrap_or("hook");
            let outcome = non_empty_str(parsed.get("outcome")).unwrap_or("completed");
            let detail = non_empty_str(parsed.get("output"))
                .or_else(|| non_empty_str(parsed.get("stdout")))
                .or_else(|| non_empty_str(parsed.get("stderr")));
            Some(match detail {
                Some(detail) => format!("[hook:{hook_name}] {outcome}: {detail}"),
                None => format!("[hook:{hook_name}] {outcome}"),
            })
        }
        "task_started" => {
            let description = non_empty_str(parsed.get("description")).unwrap_or("task started");
            Some(format!("[task] {description}"))
        }
        "task_progress" => {
            let detail = non_empty_str(parsed.get("summary"))
                .or_else(|| non_empty_str(parsed.get("description")))
                .unwrap_or("task in progress");
            Some(format!("[task] {detail}"))
        }
        "task_notification" => {
            let status = non_empty_str(parsed.get("status")).unwrap_or("updated");
            let summary = non_empty_str(parsed.get("summary")).unwrap_or("task update");
            Some(format!("[task:{status}] {summary}"))
        }
        "session_state_changed" => {
            let state = non_empty_str(parsed.get("state")).unwrap_or("unknown");
            Some(format!("[session] state={state}"))
        }
        "files_persisted" => {
            let count = parsed
                .get("files")
                .and_then(|v| v.as_array())
                .map(|files| files.len())
                .unwrap_or(0);
            Some(format!("[session] persisted {count} file(s)"))
        }
        "local_command_output" => {
            non_empty_str(parsed.get("content")).map(|content| format!("[local] {content}"))
        }
        "compact_boundary" => {
            let trigger = parsed
                .get("compact_metadata")
                .and_then(|v| v.get("trigger"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            Some(format!("[session] compacted ({trigger})"))
        }
        "api_retry" => {
            let attempt = parsed.get("attempt").and_then(|v| v.as_u64());
            let max_retries = parsed.get("max_retries").and_then(|v| v.as_u64());
            let retry_delay_ms = parsed.get("retry_delay_ms").and_then(|v| v.as_u64());
            let error_message = parsed
                .get("error")
                .and_then(|v| v.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("API request failed");

            match (attempt, max_retries, retry_delay_ms) {
                (Some(attempt), Some(max), Some(delay)) => Some(format!(
                    "[api:retry] {error_message} (attempt {attempt}/{max}, retrying in {}s)",
                    delay / 1000
                )),
                (Some(attempt), Some(max), None) => Some(format!(
                    "[api:retry] {error_message} (attempt {attempt}/{max})"
                )),
                _ => Some(format!("[api:retry] {error_message}")),
            }
        }
        "post_turn_summary" => non_empty_str(parsed.get("status_detail"))
            .or_else(|| non_empty_str(parsed.get("summary")))
            .map(|summary| format!("[turn:summary] {summary}")),
        _ => None,
    }
}

fn summarize_tool_progress_event(parsed: &Value) -> Option<String> {
    let tool_name = non_empty_str(parsed.get("tool_name")).unwrap_or("tool");
    let elapsed = parsed
        .get("elapsed_time_seconds")
        .and_then(|v| v.as_f64())
        .or_else(|| {
            parsed
                .get("elapsed_time_seconds")
                .and_then(|v| v.as_u64().map(|n| n as f64))
        });
    let task_id = non_empty_str(parsed.get("task_id"));

    match (elapsed, task_id) {
        (Some(elapsed), Some(task_id)) => Some(format!(
            "[{tool_name}] running {:.0}s (task {task_id})",
            elapsed
        )),
        (Some(elapsed), None) => Some(format!("[{tool_name}] running {:.0}s", elapsed)),
        (None, Some(task_id)) => Some(format!("[{tool_name}] task {task_id}")),
        (None, None) => Some(format!("[{tool_name}] running")),
    }
}

fn summarize_tool_use_summary_event(parsed: &Value) -> Option<String> {
    non_empty_str(parsed.get("summary")).map(|summary| summary.to_string())
}

fn summarize_auth_status_event(parsed: &Value) -> Option<String> {
    if let Some(error) = non_empty_str(parsed.get("error")) {
        return Some(format!("[auth:error] {error}"));
    }

    parsed
        .get("output")
        .and_then(|v| v.as_array())
        .and_then(|lines| {
            lines
                .iter()
                .filter_map(|line| line.as_str().map(str::trim))
                .find(|line| !line.is_empty())
        })
        .map(|line| format!("[auth] {line}"))
}

fn result_subtype(parsed: &Value) -> Option<&str> {
    non_empty_str(parsed.get("subtype"))
}

fn result_text(parsed: &Value) -> String {
    parsed
        .get("result")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn result_is_error(parsed: &Value) -> bool {
    parsed
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| result_subtype(parsed).is_some_and(|subtype| subtype != "success"))
}

fn result_is_max_turns(parsed: &Value) -> bool {
    result_subtype(parsed) == Some("error_max_turns")
}

fn result_error_message(parsed: &Value) -> String {
    let errors: Vec<&str> = parsed
        .get("errors")
        .and_then(|v| v.as_array())
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::trim))
                .filter(|message| !message.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let detail = if errors.is_empty() {
        let text = result_text(parsed);
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    } else {
        Some(errors.join("; "))
    };

    match (result_subtype(parsed), detail) {
        (Some("success") | None, Some(detail)) => detail,
        (Some(subtype), Some(detail)) => format!("{subtype}: {detail}"),
        (Some(subtype), None) => format!("Claude CLI returned {subtype}"),
        (None, None) => "Claude CLI returned an error".to_string(),
    }
}

/// Stream a complete Claude CLI session, parsing line-delimited JSON events.
///
/// The Claude CLI with `--output-format stream-json` emits one JSON object
/// per line to stdout. Each object has a `"type"` field indicating the
/// event kind. This function reads all events until the process exits or
/// a `"result"` event is received.
pub async fn stream_claude_session(
    process: &mut AgentProcess,
    event_tx: &mpsc::Sender<AgentEvent>,
    session_timeout: Duration,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<TurnResult, SymphonyError> {
    info!("streaming Claude CLI session output");

    let result = tokio::time::timeout(session_timeout, async {
        stream_claude_inner(process, event_tx, cancel_rx).await
    })
    .await;

    match result {
        Ok(inner) => inner,
        Err(_elapsed) => {
            warn!(
                timeout_ms = session_timeout.as_millis(),
                "Claude CLI session timed out"
            );
            let _ = event_tx
                .send(AgentEvent::TurnFailed {
                    error: "claude session timed out".to_string(),
                    timestamp: Utc::now(),
                })
                .await;
            Ok(TurnResult::TimedOut)
        }
    }
}

async fn stream_claude_inner(
    process: &mut AgentProcess,
    event_tx: &mpsc::Sender<AgentEvent>,
    cancel_rx: &mut watch::Receiver<bool>,
) -> Result<TurnResult, SymphonyError> {
    let mut got_result = false;
    let mut final_result = TurnResult::ProcessExited;
    let mut last_message_at = std::time::Instant::now();

    loop {
        let line = match tokio::select! {
            changed = cancel_rx.changed() => {
                match changed {
                    Ok(()) if *cancel_rx.borrow() => {
                        info!("Claude CLI session cancelled by orchestrator");
                        let _ = process.kill().await;
                        let _ = event_tx
                            .send(AgentEvent::TurnCancelled { timestamp: Utc::now() })
                            .await;
                        return Ok(TurnResult::Cancelled);
                    }
                    Ok(()) => continue,
                    Err(_) => continue,
                }
            }
            result = tokio::time::timeout(Duration::from_secs(60), process.read_line()) => result,
        } {
            Ok(result) => match result? {
                Some(line) if line.is_empty() => continue,
                Some(line) => {
                    last_message_at = std::time::Instant::now();
                    line
                }
                None => {
                    info!("Claude CLI process exited (EOF)");
                    break;
                }
            },
            Err(_) => {
                let idle_secs = last_message_at.elapsed().as_secs();
                match process.try_wait().await {
                    Ok(Some(status)) => {
                        info!(status = ?status, "Claude CLI exited during idle wait");
                        break;
                    }
                    Ok(None) => {
                        info!(idle_secs, "Claude CLI still running, waiting for output");
                    }
                    Err(e) => {
                        warn!(error = %e, "failed to check Claude CLI status");
                    }
                }
                continue;
            }
        };

        let parsed: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                debug!(
                    line = %line.chars().take(200).collect::<String>(),
                    "non-JSON output from Claude CLI"
                );
                continue;
            }
        };

        let event_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

        debug!(event_type, "Claude CLI event received");

        match event_type {
            "system" => {
                if let Some(message) = summarize_system_event(&parsed) {
                    emit_notification(event_tx, message).await;
                }
                if let Some(session_id) = parsed.get("session_id").and_then(|v| v.as_str()) {
                    debug!(session_id, "Claude CLI system event");
                }
            }

            "assistant" => {
                handle_assistant_event(&parsed, event_tx).await;
            }

            "tool" => {
                let tool_name = parsed
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let input_summary = summarize_tool_input(parsed.get("input"));
                let msg = match input_summary {
                    Some(summary) => format!("[{tool_name}] {summary}"),
                    None => format!("[{tool_name}]"),
                };
                emit_notification(event_tx, msg).await;
            }

            "tool_progress" => {
                if let Some(message) = summarize_tool_progress_event(&parsed) {
                    emit_notification(event_tx, message).await;
                }
            }

            "tool_use_summary" => {
                if let Some(message) = summarize_tool_use_summary_event(&parsed) {
                    emit_notification(event_tx, message).await;
                }
            }

            "auth_status" => {
                if let Some(message) = summarize_auth_status_event(&parsed) {
                    emit_notification(event_tx, message).await;
                }
            }

            "rate_limit_event" => {
                if let Some(info) = parsed.get("rate_limit_info") {
                    let _ = event_tx
                        .send(AgentEvent::RateLimitUpdate {
                            rate_limits: info.clone(),
                            timestamp: Utc::now(),
                        })
                        .await;
                }
            }

            "result" => {
                got_result = true;
                let is_error = result_is_error(&parsed);

                // Extract and emit final usage.
                if let Some(usage) = extract_claude_usage(&parsed) {
                    let _ = event_tx
                        .send(AgentEvent::TokenUsageUpdate {
                            usage: usage.to_token_usage(),
                            timestamp: Utc::now(),
                        })
                        .await;
                }

                let result_text = result_text(&parsed);

                // Extract cost and duration for logging.
                let cost = parsed.get("total_cost_usd").and_then(|v| v.as_f64());
                let duration_ms = parsed.get("duration_ms").and_then(|v| v.as_u64());
                let num_turns = parsed.get("num_turns").and_then(|v| v.as_u64());
                if let Some(c) = cost {
                    info!(
                        cost_usd = c,
                        duration_ms = ?duration_ms,
                        num_turns = ?num_turns,
                        "Claude CLI session completed"
                    );
                }

                if is_error && result_is_max_turns(&parsed) {
                    let error_msg = result_error_message(&parsed);
                    emit_notification(
                        event_tx,
                        format!("{error_msg}; continuing if Symphony turns remain"),
                    )
                    .await;
                    let _ = event_tx
                        .send(AgentEvent::TurnCompleted {
                            timestamp: Utc::now(),
                            usage: extract_claude_usage(&parsed).map(|u| u.to_token_usage()),
                        })
                        .await;
                    final_result = TurnResult::MaxTurns;
                } else if is_error {
                    let error_msg = result_error_message(&parsed);
                    let _ = event_tx
                        .send(AgentEvent::TurnFailed {
                            error: error_msg.clone(),
                            timestamp: Utc::now(),
                        })
                        .await;
                    final_result = TurnResult::Failed(error_msg);
                } else {
                    if !result_text.is_empty() {
                        emit_notification(event_tx, result_text).await;
                    }
                    let _ = event_tx
                        .send(AgentEvent::TurnCompleted {
                            timestamp: Utc::now(),
                            usage: extract_claude_usage(&parsed).map(|u| u.to_token_usage()),
                        })
                        .await;
                    final_result = TurnResult::Completed;
                }

                // SDK `result` messages are the terminal session signal. Do
                // not wait for extra stdout/EOF after it; some launchers
                // intentionally kill the CLI as soon as result is delivered.
                break;
            }

            other => {
                debug!(event_type = other, "unhandled Claude CLI event type");
                let _ = event_tx
                    .send(AgentEvent::OtherMessage {
                        raw: parsed.clone(),
                        timestamp: Utc::now(),
                    })
                    .await;
            }
        }
    }

    if !got_result {
        // Process exited without a result event. Check exit status.
        match process.try_wait().await {
            Ok(Some(status)) if status.success() => {
                info!("Claude CLI exited successfully without result event");
                final_result = TurnResult::Completed;
            }
            Ok(Some(status)) => {
                let msg = format!("Claude CLI exited with status: {status}");
                warn!(msg);
                final_result = TurnResult::Failed(msg);
            }
            _ => {
                warn!("Claude CLI EOF without result event or exit status");
                final_result = TurnResult::ProcessExited;
            }
        }
    }

    Ok(final_result)
}

/// Handle an "assistant" event from the Claude CLI stream.
///
/// Parses `message.content` array for text and tool_use blocks,
/// and extracts usage information.
async fn handle_assistant_event(parsed: &Value, event_tx: &mpsc::Sender<AgentEvent>) {
    // Extract content blocks from message.content.
    let content = parsed
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array());

    if let Some(blocks) = content {
        let mut text_parts = Vec::new();

        for block in blocks {
            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match block_type {
                "text" => {
                    if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                        text_parts.push(text.to_string());
                    }
                }
                "tool_use" => {
                    let tool_name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown");
                    let input_summary = summarize_tool_input(block.get("input"));
                    let msg = match input_summary {
                        Some(summary) => format!("[{tool_name}] {summary}"),
                        None => format!("[{tool_name}]"),
                    };
                    emit_notification(event_tx, msg).await;
                }
                "tool_result" => {
                    // Skip tool_result notifications - they're verbose and
                    // the tool_use already shows what happened.
                }
                _ => {
                    debug!(block_type, "unknown content block type");
                }
            }
        }

        if !text_parts.is_empty() {
            let combined = text_parts.join("\n");
            emit_notification(event_tx, combined).await;
        }
    }

    // Extract usage from message.usage.
    let usage = parsed.get("message").and_then(|m| m.get("usage"));

    if let Some(usage_val) = usage {
        if let Ok(claude_usage) = serde_json::from_value::<ClaudeUsage>(usage_val.clone()) {
            let _ = event_tx
                .send(AgentEvent::TokenUsageUpdate {
                    usage: claude_usage.to_token_usage(),
                    timestamp: Utc::now(),
                })
                .await;
        }
    }
}

/// Extract usage from a result or assistant event.
fn extract_claude_usage(parsed: &Value) -> Option<ClaudeUsage> {
    let usage_val = parsed
        .get("usage")
        .or_else(|| parsed.get("message").and_then(|m| m.get("usage")))?;
    serde_json::from_value::<ClaudeUsage>(usage_val.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn parse_claude_usage_full() {
        let val = json!({
            "input_tokens": 100,
            "output_tokens": 200,
            "cache_creation_input_tokens": 50,
            "cache_read_input_tokens": 30
        });
        let usage: ClaudeUsage = serde_json::from_value(val).unwrap();
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(200));
        assert_eq!(usage.cache_creation_input_tokens, Some(50));
        assert_eq!(usage.cache_read_input_tokens, Some(30));

        let token_usage = usage.to_token_usage();
        assert_eq!(token_usage.input_tokens, 180); // 100 + 50 + 30
        assert_eq!(token_usage.output_tokens, 200);
        assert_eq!(token_usage.total_tokens, 380);
    }

    #[test]
    fn parse_claude_usage_partial() {
        let val = json!({
            "input_tokens": 100,
            "output_tokens": 200
        });
        let usage: ClaudeUsage = serde_json::from_value(val).unwrap();
        let token_usage = usage.to_token_usage();
        assert_eq!(token_usage.input_tokens, 100);
        assert_eq!(token_usage.output_tokens, 200);
        assert_eq!(token_usage.total_tokens, 300);
    }

    #[test]
    fn extract_usage_from_result_event() {
        let event = json!({
            "type": "result",
            "result": "Done",
            "is_error": false,
            "usage": {
                "input_tokens": 500,
                "output_tokens": 300
            }
        });
        let usage = extract_claude_usage(&event).unwrap();
        assert_eq!(usage.input_tokens, Some(500));
        assert_eq!(usage.output_tokens, Some(300));
    }

    #[test]
    fn extract_usage_from_assistant_event() {
        let event = json!({
            "type": "assistant",
            "message": {
                "content": [{"type": "text", "text": "hello"}],
                "usage": {
                    "input_tokens": 50,
                    "output_tokens": 25
                }
            }
        });
        let usage = extract_claude_usage(&event).unwrap();
        assert_eq!(usage.input_tokens, Some(50));
    }

    #[test]
    fn extract_usage_missing_returns_none() {
        let event = json!({"type": "system", "message": "init"});
        assert!(extract_claude_usage(&event).is_none());
    }

    #[test]
    fn parse_result_success() {
        let event = json!({
            "type": "result",
            "result": "All tests pass",
            "is_error": false,
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 500
            }
        });
        let is_error = event
            .get("is_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        assert!(!is_error);
        let result_text = event.get("result").and_then(|v| v.as_str()).unwrap_or("");
        assert_eq!(result_text, "All tests pass");
    }

    #[test]
    fn parse_result_error() {
        let event = json!({
            "type": "result",
            "result": "Command failed",
            "is_error": true,
            "usage": {
                "input_tokens": 200,
                "output_tokens": 50
            }
        });
        let is_error = event
            .get("is_error")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        assert!(is_error);
    }

    #[test]
    fn truncate_notification_handles_multibyte_text() {
        let message = "\u{1f642}".repeat(MAX_NOTIFICATION_CHARS + 1);
        let truncated = truncate_notification(message);

        assert!(truncated.ends_with("..."));
        assert_eq!(
            truncated.trim_end_matches('.').chars().count(),
            MAX_NOTIFICATION_CHARS
        );
    }

    #[test]
    fn result_error_message_uses_sdk_errors_array() {
        let event = json!({
            "type": "result",
            "subtype": "error_max_turns",
            "is_error": true,
            "errors": ["Reached maximum number of turns (20)"],
            "usage": {
                "input_tokens": 200,
                "output_tokens": 50
            }
        });

        assert!(result_is_error(&event));
        assert_eq!(
            result_error_message(&event),
            "error_max_turns: Reached maximum number of turns (20)"
        );
        assert!(result_is_max_turns(&event));
    }

    #[test]
    fn result_error_message_falls_back_to_result_text() {
        let event = json!({
            "type": "result",
            "subtype": "success",
            "result": "API error response",
            "is_error": true
        });

        assert!(result_is_error(&event));
        assert_eq!(result_error_message(&event), "API error response");
    }

    #[test]
    fn result_subtype_detects_error_without_is_error_field() {
        let event = json!({
            "type": "result",
            "subtype": "error_during_execution",
            "errors": ["Execution error"]
        });

        assert!(result_is_error(&event));
        assert_eq!(
            result_error_message(&event),
            "error_during_execution: Execution error"
        );
    }

    #[test]
    fn parse_system_event() {
        let event = json!({
            "type": "system",
            "subtype": "init",
            "session_id": "abc-123",
            "model": "claude-sonnet-4-6",
            "cwd": "/repo"
        });
        assert_eq!(
            summarize_system_event(&event).as_deref(),
            Some("[session:init] model=claude-sonnet-4-6 cwd=/repo")
        );
    }

    #[test]
    fn parse_assistant_with_text_blocks() {
        let event = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "Hello world"},
                    {"type": "text", "text": "Second block"}
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5
                }
            }
        });
        let content = event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(
            content[0].get("text").and_then(|v| v.as_str()),
            Some("Hello world")
        );
    }

    #[test]
    fn parse_assistant_with_tool_use() {
        let event = json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "tool_use", "name": "bash", "input": {"command": "ls"}}
                ],
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5
                }
            }
        });
        let content = event
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
            .unwrap();
        assert_eq!(
            content[0].get("type").and_then(|v| v.as_str()),
            Some("tool_use")
        );
        assert_eq!(
            content[0].get("name").and_then(|v| v.as_str()),
            Some("bash")
        );
    }

    #[test]
    fn summarize_hook_response_event() {
        let event = json!({
            "type": "system",
            "subtype": "hook_response",
            "hook_name": "PreToolUse",
            "outcome": "success",
            "output": "validated"
        });
        assert_eq!(
            summarize_system_event(&event).as_deref(),
            Some("[hook:PreToolUse] success: validated")
        );
    }

    #[test]
    fn summarize_api_retry_event() {
        let event = json!({
            "type": "system",
            "subtype": "api_retry",
            "attempt": 2,
            "max_retries": 5,
            "retry_delay_ms": 3000,
            "error": {"message": "Rate limit exceeded"}
        });
        assert_eq!(
            summarize_system_event(&event).as_deref(),
            Some("[api:retry] Rate limit exceeded (attempt 2/5, retrying in 3s)")
        );
    }

    #[test]
    fn summarize_auth_status_event_uses_error_first() {
        let event = json!({
            "type": "auth_status",
            "isAuthenticating": false,
            "output": ["Checking credentials"],
            "error": "Authentication failed"
        });
        assert_eq!(
            summarize_auth_status_event(&event).as_deref(),
            Some("[auth:error] Authentication failed")
        );
    }

    #[test]
    fn summarize_tool_progress_event_with_task_id() {
        let event = json!({
            "type": "tool_progress",
            "tool_name": "Bash",
            "elapsed_time_seconds": 12.7,
            "task_id": "task-42"
        });
        assert_eq!(
            summarize_tool_progress_event(&event).as_deref(),
            Some("[Bash] running 13s (task task-42)")
        );
    }

    #[test]
    fn summarize_tool_use_summary_event_passes_through_summary() {
        let event = json!({
            "type": "tool_use_summary",
            "summary": "Read 3 files, ran 2 commands"
        });
        assert_eq!(
            summarize_tool_use_summary_event(&event).as_deref(),
            Some("Read 3 files, ran 2 commands")
        );
    }

    #[tokio::test]
    async fn stream_returns_when_result_event_is_seen() {
        let dir = TempDir::new().unwrap();
        let command = "printf '%s\n' '{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"result\":\"done\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}'; sleep 5";
        let mut process = AgentProcess::launch(command, dir.path(), &[], false)
            .await
            .unwrap();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            stream_claude_session(
                &mut process,
                &event_tx,
                Duration::from_secs(30),
                &mut cancel_rx,
            ),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(matches!(result, TurnResult::Completed));
        let _ = process.kill().await;

        let mut saw_completed = false;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(event, AgentEvent::TurnCompleted { .. }) {
                saw_completed = true;
            }
        }
        assert!(saw_completed);
    }

    #[tokio::test]
    async fn stream_distinguishes_max_turns_from_success() {
        let dir = TempDir::new().unwrap();
        let command = "printf '%s\n' '{\"type\":\"result\",\"subtype\":\"error_max_turns\",\"is_error\":true,\"errors\":[\"Reached maximum number of turns (20)\"],\"usage\":{\"input_tokens\":1,\"output_tokens\":2}}'; sleep 5";
        let mut process = AgentProcess::launch(command, dir.path(), &[], false)
            .await
            .unwrap();
        let (event_tx, mut event_rx) = mpsc::channel(8);
        let (_cancel_tx, mut cancel_rx) = watch::channel(false);

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            stream_claude_session(
                &mut process,
                &event_tx,
                Duration::from_secs(30),
                &mut cancel_rx,
            ),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(matches!(result, TurnResult::MaxTurns));
        let _ = process.kill().await;

        let mut saw_completed = false;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(event, AgentEvent::TurnCompleted { .. }) {
                saw_completed = true;
            }
        }
        assert!(saw_completed);
    }
}
