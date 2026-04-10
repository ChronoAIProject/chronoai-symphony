use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const NOTE_TOOL: &str = "symphony_note";
const MAILBOX_TOOL: &str = "symphony_mailbox";
const CLAIM_TOOL: &str = "symphony_claim";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicToolContext {
    pub coordination_api_url: String,
    pub issue_id: String,
    pub stage_role: Option<String>,
    pub workspace_dir: PathBuf,
}

pub fn context_from_env_vars(
    workspace_path: &Path,
    env_vars: &[(String, String)],
) -> Option<DynamicToolContext> {
    let coordination_api_url = env_value(env_vars, "SYMPHONY_COORDINATION_API_URL")?;
    let issue_id = env_value(env_vars, "SYMPHONY_ISSUE_ID")?;
    if coordination_api_url.trim().is_empty() || issue_id.trim().is_empty() {
        return None;
    }

    Some(DynamicToolContext {
        coordination_api_url,
        issue_id,
        stage_role: env_value(env_vars, "SYMPHONY_STAGE_ROLE")
            .filter(|value| !value.trim().is_empty()),
        workspace_dir: workspace_path.to_path_buf(),
    })
}

pub fn coordination_tool_specs() -> Vec<Value> {
    vec![note_tool_spec(), mailbox_tool_spec(), claim_tool_spec()]
}

pub fn supports_tool(tool_name: &str) -> bool {
    matches!(tool_name.trim(), NOTE_TOOL | MAILBOX_TOOL | CLAIM_TOOL)
}

pub async fn execute(tool_name: &str, arguments: Value, context: &DynamicToolContext) -> Value {
    match tool_name.trim() {
        NOTE_TOOL => execute_note(arguments, context).await,
        MAILBOX_TOOL => execute_mailbox(arguments, context).await,
        CLAIM_TOOL => execute_claim(arguments, context).await,
        other => failure_response(format!(
            "Unsupported Symphony dynamic tool: {other}. Supported tools: {NOTE_TOOL}, {MAILBOX_TOOL}, {CLAIM_TOOL}."
        )),
    }
}

fn note_tool_spec() -> Value {
    json!({
        "name": NOTE_TOOL,
        "description": "Append a durable coordination note or handoff into Symphony's shared coordination surface for this issue.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["target", "message"],
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Relative coordination file path under .symphony/coordination/, such as .symphony/coordination/shared.md or .symphony/coordination/handoffs.md."
                },
                "message": {
                    "type": "string",
                    "description": "Short factual note to append."
                }
            }
        }
    })
}

fn mailbox_tool_spec() -> Value {
    json!({
        "name": MAILBOX_TOOL,
        "description": "Read, send, or acknowledge targeted Symphony coordination messages between active roles on this issue.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["read", "send", "ack"],
                    "description": "Mailbox operation to perform."
                },
                "role": {
                    "type": "string",
                    "description": "Target role for send, or mailbox role for read/ack. Defaults to the current stage role when omitted."
                },
                "message": {
                    "type": "string",
                    "description": "Message body required for send."
                },
                "message_id": {
                    "type": "string",
                    "description": "Message ID required for ack."
                },
                "include_read": {
                    "type": "boolean",
                    "description": "When action=read, include already acknowledged messages."
                }
            }
        }
    })
}

fn claim_tool_spec() -> Value {
    json!({
        "name": CLAIM_TOOL,
        "description": "Claim, release, or list temporary ownership over a shared scope so parallel agents do not stomp each other.",
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "required": ["action"],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["claim", "release", "list"],
                    "description": "Claim operation to perform."
                },
                "scope": {
                    "type": "string",
                    "description": "Shared scope path for claim or release."
                },
                "note": {
                    "type": "string",
                    "description": "Short reason for the claim."
                }
            }
        }
    })
}

async fn execute_note(arguments: Value, context: &DynamicToolContext) -> Value {
    let Some(target) = string_arg(&arguments, "target") else {
        return failure_response("symphony_note requires a non-empty 'target'.".to_string());
    };
    let Some(message) = string_arg(&arguments, "message") else {
        return failure_response("symphony_note requires a non-empty 'message'.".to_string());
    };

    let url = coordination_url(context, "notes/append");
    let role = current_role(context);
    let workspace_dir = context.workspace_dir.to_string_lossy().into_owned();
    let form = [
        ("issue_id", context.issue_id.as_str()),
        ("role", role.as_str()),
        ("workspace_dir", workspace_dir.as_str()),
        ("target", target.as_str()),
        ("message", message.as_str()),
    ];

    match post_form_text(&url, &form).await {
        Ok(output) => success_response(output),
        Err(output) => failure_response(output),
    }
}

async fn execute_mailbox(arguments: Value, context: &DynamicToolContext) -> Value {
    let Some(action) = string_arg(&arguments, "action") else {
        return failure_response("symphony_mailbox requires a non-empty 'action'.".to_string());
    };
    let role = string_arg(&arguments, "role").unwrap_or_else(|| current_role(context));

    match action.as_str() {
        "read" => {
            let include_read = bool_arg(&arguments, "include_read").unwrap_or(false);
            let url = coordination_url(context, "mailbox/read");
            let query = [
                ("issue_id", context.issue_id.as_str()),
                ("role", role.as_str()),
                ("all", if include_read { "true" } else { "false" }),
            ];
            match get_query_text(&url, &query).await {
                Ok(output) => success_response(output),
                Err(output) => failure_response(output),
            }
        }
        "send" => {
            let Some(message) = string_arg(&arguments, "message") else {
                return failure_response(
                    "symphony_mailbox action=send requires a non-empty 'message'.".to_string(),
                );
            };
            let url = coordination_url(context, "mailbox/send");
            let sender = current_role(context);
            let workspace_dir = context.workspace_dir.to_string_lossy().into_owned();
            let form = [
                ("issue_id", context.issue_id.as_str()),
                ("from_role", sender.as_str()),
                ("to_role", role.as_str()),
                ("body", message.as_str()),
                ("workspace_dir", workspace_dir.as_str()),
            ];
            match post_form_text(&url, &form).await {
                Ok(output) => success_response(output),
                Err(output) => failure_response(output),
            }
        }
        "ack" => {
            let Some(message_id) = string_arg(&arguments, "message_id") else {
                return failure_response(
                    "symphony_mailbox action=ack requires a non-empty 'message_id'.".to_string(),
                );
            };
            let url = coordination_url(context, "mailbox/ack");
            let workspace_dir = context.workspace_dir.to_string_lossy().into_owned();
            let form = [
                ("issue_id", context.issue_id.as_str()),
                ("role", role.as_str()),
                ("message_id", message_id.as_str()),
                ("workspace_dir", workspace_dir.as_str()),
            ];
            match post_form_text(&url, &form).await {
                Ok(output) => success_response(output),
                Err(output) => failure_response(output),
            }
        }
        other => failure_response(format!(
            "symphony_mailbox action must be one of read, send, ack; got '{other}'."
        )),
    }
}

async fn execute_claim(arguments: Value, context: &DynamicToolContext) -> Value {
    let Some(action) = string_arg(&arguments, "action") else {
        return failure_response("symphony_claim requires a non-empty 'action'.".to_string());
    };
    let owner = current_role(context);

    match action.as_str() {
        "list" => {
            let url = coordination_url(context, "claims/list");
            let query = [("issue_id", context.issue_id.as_str())];
            match get_query_text(&url, &query).await {
                Ok(output) => success_response(output),
                Err(output) => failure_response(output),
            }
        }
        "claim" => {
            let Some(scope) = string_arg(&arguments, "scope") else {
                return failure_response(
                    "symphony_claim action=claim requires a non-empty 'scope'.".to_string(),
                );
            };
            let note = string_arg(&arguments, "note").unwrap_or_default();
            let url = coordination_url(context, "claims/claim");
            let workspace_dir = context.workspace_dir.to_string_lossy().into_owned();
            let form = [
                ("issue_id", context.issue_id.as_str()),
                ("owner_role", owner.as_str()),
                ("scope", scope.as_str()),
                ("note", note.as_str()),
                ("workspace_dir", workspace_dir.as_str()),
            ];
            match post_form_text(&url, &form).await {
                Ok(output) => success_response(output),
                Err(output) => failure_response(output),
            }
        }
        "release" => {
            let Some(scope) = string_arg(&arguments, "scope") else {
                return failure_response(
                    "symphony_claim action=release requires a non-empty 'scope'.".to_string(),
                );
            };
            let url = coordination_url(context, "claims/release");
            let workspace_dir = context.workspace_dir.to_string_lossy().into_owned();
            let form = [
                ("issue_id", context.issue_id.as_str()),
                ("owner_role", owner.as_str()),
                ("scope", scope.as_str()),
                ("workspace_dir", workspace_dir.as_str()),
            ];
            match post_form_text(&url, &form).await {
                Ok(output) => success_response(output),
                Err(output) => failure_response(output),
            }
        }
        other => failure_response(format!(
            "symphony_claim action must be one of claim, release, list; got '{other}'."
        )),
    }
}

fn env_value(env_vars: &[(String, String)], key: &str) -> Option<String> {
    env_vars
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.clone())
}

fn current_role(context: &DynamicToolContext) -> String {
    context
        .stage_role
        .as_deref()
        .unwrap_or("general")
        .trim()
        .to_lowercase()
}

fn coordination_url(context: &DynamicToolContext, suffix: &str) -> String {
    format!(
        "{}/{}",
        context.coordination_api_url.trim_end_matches('/'),
        suffix
    )
}

fn string_arg(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn bool_arg(arguments: &Value, key: &str) -> Option<bool> {
    arguments.get(key).and_then(|value| value.as_bool())
}

async fn post_form_text(url: &str, form: &[(&str, &str)]) -> Result<String, String> {
    let body = encode_form_pairs(form);
    let response = reqwest::Client::new()
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body)
        .send()
        .await
        .map_err(|error| format!("failed to call Symphony coordination API: {error}"))?;
    response_text(response).await
}

async fn get_query_text(url: &str, query: &[(&str, &str)]) -> Result<String, String> {
    let encoded = encode_form_pairs(query);
    let request_url = if encoded.is_empty() {
        url.to_string()
    } else {
        format!("{url}?{encoded}")
    };
    let response = reqwest::Client::new()
        .get(request_url)
        .send()
        .await
        .map_err(|error| format!("failed to call Symphony coordination API: {error}"))?;
    response_text(response).await
}

async fn response_text(response: reqwest::Response) -> Result<String, String> {
    let status = response.status();
    let body = response.text().await.unwrap_or_else(|error| {
        format!("failed to read Symphony coordination API response: {error}")
    });
    if status.is_success() {
        Ok(body)
    } else {
        Err(format!(
            "Symphony coordination API returned {}: {}",
            status.as_u16(),
            body.trim()
        ))
    }
}

fn success_response(output: String) -> Value {
    dynamic_tool_response(true, output)
}

fn failure_response(output: String) -> Value {
    dynamic_tool_response(false, output)
}

fn dynamic_tool_response(success: bool, output: String) -> Value {
    json!({
        "success": success,
        "output": output,
        "contentItems": [
            {
                "type": "inputText",
                "text": output
            }
        ]
    })
}

fn encode_form_pairs(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_requires_issue_and_api_url() {
        let workspace = Path::new("/tmp/workspace");
        assert!(context_from_env_vars(workspace, &[]).is_none());
        assert!(
            context_from_env_vars(
                workspace,
                &[(
                    "SYMPHONY_COORDINATION_API_URL".to_string(),
                    "http://127.0.0.1".to_string()
                )]
            )
            .is_none()
        );
    }

    #[test]
    fn context_uses_workspace_and_role() {
        let workspace = Path::new("/tmp/workspace");
        let context = context_from_env_vars(
            workspace,
            &[
                (
                    "SYMPHONY_COORDINATION_API_URL".to_string(),
                    "http://127.0.0.1:7777/api/v1/coordination".to_string(),
                ),
                ("SYMPHONY_ISSUE_ID".to_string(), "42".to_string()),
                ("SYMPHONY_STAGE_ROLE".to_string(), "Reviewer".to_string()),
            ],
        )
        .expect("context");

        assert_eq!(context.issue_id, "42");
        assert_eq!(context.stage_role.as_deref(), Some("Reviewer"));
        assert_eq!(context.workspace_dir, workspace);
    }

    #[test]
    fn coordination_tool_specs_expose_expected_tools() {
        let names: Vec<_> = coordination_tool_specs()
            .into_iter()
            .filter_map(|spec| {
                spec.get("name")
                    .and_then(|value| value.as_str())
                    .map(ToString::to_string)
            })
            .collect();
        assert_eq!(names, vec![NOTE_TOOL, MAILBOX_TOOL, CLAIM_TOOL]);
    }

    #[test]
    fn supports_tool_only_for_symphony_coordination_tools() {
        assert!(supports_tool(NOTE_TOOL));
        assert!(supports_tool(MAILBOX_TOOL));
        assert!(supports_tool(CLAIM_TOOL));
        assert!(!supports_tool("linear_graphql"));
    }
}
