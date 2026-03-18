//! Native Claude Code CLI adapter.
//!
//! Parses streaming JSON output from `claude -p --output-format stream-json`.
//! Unlike the Codex JSON-RPC protocol, there is no handshake or multi-turn
//! loop -- the CLI runs a single invocation and streams structured events
//! to stdout.

use std::time::Duration;

use chrono::Utc;
use serde::Deserialize;
use serde_json::Value;
use symphony_core::error::SymphonyError;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::process::AgentProcess;
use crate::protocol::events::{AgentEvent, TokenUsage};
use crate::protocol::streaming::TurnResult;

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
) -> Result<TurnResult, SymphonyError> {
    info!("streaming Claude CLI session output");

    let result = tokio::time::timeout(session_timeout, async {
        stream_claude_inner(process, event_tx).await
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
) -> Result<TurnResult, SymphonyError> {
    let mut got_result = false;
    let mut final_result = TurnResult::ProcessExited;
    let mut last_message_at = std::time::Instant::now();

    loop {
        let line = match tokio::time::timeout(
            Duration::from_secs(60),
            process.read_line(),
        )
        .await
        {
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
                        info!(
                            idle_secs,
                            "Claude CLI still running, waiting for output"
                        );
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

        let event_type = parsed
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        debug!(event_type, "Claude CLI event received");

        match event_type {
            "system" => {
                let session_id = parsed
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("claude-session")
                    .to_string();
                let message = parsed
                    .get("message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Claude CLI session started")
                    .to_string();
                let _ = event_tx
                    .send(AgentEvent::Notification {
                        message: format!("[system] {message}"),
                        timestamp: Utc::now(),
                    })
                    .await;
                debug!(session_id, "Claude CLI system event");
            }

            "assistant" => {
                handle_assistant_event(&parsed, event_tx).await;
            }

            "tool" => {
                let tool_name = parsed
                    .get("tool")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let _ = event_tx
                    .send(AgentEvent::Notification {
                        message: format!("[tool_use] {tool_name}"),
                        timestamp: Utc::now(),
                    })
                    .await;
            }

            "result" => {
                got_result = true;
                let is_error = parsed
                    .get("is_error")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // Extract and emit final usage.
                if let Some(usage) = extract_claude_usage(&parsed) {
                    let _ = event_tx
                        .send(AgentEvent::TokenUsageUpdate {
                            usage: usage.to_token_usage(),
                            timestamp: Utc::now(),
                        })
                        .await;
                }

                let result_text = parsed
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                if is_error {
                    let error_msg = if result_text.is_empty() {
                        "Claude CLI returned an error".to_string()
                    } else {
                        result_text
                    };
                    let _ = event_tx
                        .send(AgentEvent::TurnFailed {
                            error: error_msg.clone(),
                            timestamp: Utc::now(),
                        })
                        .await;
                    final_result = TurnResult::Failed(error_msg);
                } else {
                    if !result_text.is_empty() {
                        let truncated = if result_text.len() > 500 {
                            format!("{}...", &result_text[..500])
                        } else {
                            result_text
                        };
                        let _ = event_tx
                            .send(AgentEvent::Notification {
                                message: truncated,
                                timestamp: Utc::now(),
                            })
                            .await;
                    }
                    let _ = event_tx
                        .send(AgentEvent::TurnCompleted {
                            timestamp: Utc::now(),
                            usage: extract_claude_usage(&parsed)
                                .map(|u| u.to_token_usage()),
                        })
                        .await;
                    final_result = TurnResult::Completed;
                }
            }

            other => {
                debug!(event_type = other, "unhandled Claude CLI event type");
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
async fn handle_assistant_event(
    parsed: &Value,
    event_tx: &mpsc::Sender<AgentEvent>,
) {
    // Extract content blocks from message.content.
    let content = parsed
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array());

    if let Some(blocks) = content {
        let mut text_parts = Vec::new();

        for block in blocks {
            let block_type = block
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("");

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
                    let _ = event_tx
                        .send(AgentEvent::Notification {
                            message: format!("[tool_use] {tool_name}"),
                            timestamp: Utc::now(),
                        })
                        .await;
                }
                "tool_result" => {
                    let _ = event_tx
                        .send(AgentEvent::Notification {
                            message: "[tool_result]".to_string(),
                            timestamp: Utc::now(),
                        })
                        .await;
                }
                _ => {
                    debug!(block_type, "unknown content block type");
                }
            }
        }

        if !text_parts.is_empty() {
            let combined = text_parts.join("\n");
            let truncated = if combined.len() > 500 {
                format!("{}...", &combined[..500])
            } else {
                combined
            };
            let _ = event_tx
                .send(AgentEvent::Notification {
                    message: truncated,
                    timestamp: Utc::now(),
                })
                .await;
        }
    }

    // Extract usage from message.usage.
    let usage = parsed
        .get("message")
        .and_then(|m| m.get("usage"));

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
    let usage_val = parsed.get("usage")
        .or_else(|| parsed.get("message").and_then(|m| m.get("usage")))?;
    serde_json::from_value::<ClaudeUsage>(usage_val.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let result_text = event
            .get("result")
            .and_then(|v| v.as_str())
            .unwrap_or("");
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
    fn parse_system_event() {
        let event = json!({
            "type": "system",
            "session_id": "abc-123",
            "message": "Claude Code session started"
        });
        let session_id = event
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("claude-session");
        assert_eq!(session_id, "abc-123");
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
        assert_eq!(content[0].get("type").and_then(|v| v.as_str()), Some("tool_use"));
        assert_eq!(content[0].get("name").and_then(|v| v.as_str()), Some("bash"));
    }
}
