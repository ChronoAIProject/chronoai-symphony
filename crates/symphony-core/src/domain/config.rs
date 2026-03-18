use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SymphonyError};

use super::workflow::WorkflowDefinition;

/// Configuration for hook scripts executed at various lifecycle points.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HooksConfig {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub before_remove: Option<String>,
    pub timeout_ms: u64,
}

/// Fully resolved service configuration with typed fields and applied defaults.
///
/// Built from the YAML front matter of a workflow definition. All environment
/// variable references (`$VAR`) are resolved, tilde paths are expanded, and
/// default values are applied for any omitted fields.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ServiceConfig {
    // -- tracker --
    pub tracker_kind: String,
    pub tracker_endpoint: String,
    pub tracker_api_key: String,
    pub tracker_project_slug: String,
    pub tracker_active_states: Vec<String>,
    pub tracker_terminal_states: Vec<String>,

    // -- github app (optional, alternative to api_key) --
    pub github_app_id: Option<u64>,
    pub github_app_installation_id: Option<u64>,
    pub github_app_private_key_path: Option<String>,

    // -- polling --
    pub polling_interval_ms: u64,

    // -- workspace --
    pub workspace_root: PathBuf,

    // -- hooks --
    pub hooks: HooksConfig,

    // -- agent --
    pub agent_max_concurrent: u32,
    pub agent_max_turns: u32,
    pub agent_max_retry_backoff_ms: u64,
    pub agent_max_concurrent_by_state: HashMap<String, u32>,

    // -- codex --
    pub codex_command: String,
    pub codex_approval_policy: Option<String>,
    pub codex_thread_sandbox: Option<String>,
    pub codex_turn_sandbox_policy: Option<String>,
    pub codex_turn_timeout_ms: u64,
    pub codex_read_timeout_ms: u64,
    pub codex_stall_timeout_ms: i64,

    // -- server --
    pub server_port: Option<u16>,
}

impl ServiceConfig {
    /// Build a `ServiceConfig` from a parsed `WorkflowDefinition`, applying
    /// defaults and resolving environment variables.
    pub fn from_workflow(workflow: &WorkflowDefinition) -> Result<Self> {
        let root = workflow.config.as_mapping().ok_or_else(|| {
            SymphonyError::WorkflowFrontMatterNotAMap
        })?;

        let tracker = get_mapping(root, "tracker");
        let polling = get_mapping(root, "polling");
        let workspace = get_mapping(root, "workspace");
        let hooks_map = get_mapping(root, "hooks");
        let agent = get_mapping(root, "agent");
        let codex = get_mapping(root, "codex");
        let server = get_mapping(root, "server");

        // -- tracker --
        let tracker_kind = get_str(&tracker, "kind")
            .unwrap_or_else(|| "github".to_owned());

        let default_endpoint = match tracker_kind.as_str() {
            "github" => "https://api.github.com",
            _ => "",
        };
        let tracker_endpoint = get_str(&tracker, "endpoint")
            .unwrap_or_else(|| default_endpoint.to_owned());

        let tracker_api_key = get_str(&tracker, "api_key")
            .map(|v| resolve_env_var(&v))
            .transpose()?
            .unwrap_or_default(); // Can be empty when using GitHub App auth

        let tracker_project_slug = get_str(&tracker, "project_slug")
            .ok_or(SymphonyError::MissingTrackerProjectSlug)?;

        let tracker_active_states = get_string_list(&tracker, "active_states")
            .unwrap_or_else(|| vec!["Todo".to_owned(), "In Progress".to_owned()]);

        let tracker_terminal_states = get_string_list(&tracker, "terminal_states")
            .unwrap_or_else(|| {
                vec![
                    "Closed".to_owned(),
                    "Cancelled".to_owned(),
                    "Canceled".to_owned(),
                    "Duplicate".to_owned(),
                    "Done".to_owned(),
                ]
            });

        // -- github app --
        let github_app_id = get_u64(&tracker, "app_id");
        let github_app_installation_id = get_u64(&tracker, "installation_id");
        let github_app_private_key_path = get_str(&tracker, "private_key_path")
            .map(|v| resolve_env_var(&v))
            .transpose()?;

        // -- polling --
        let polling_interval_ms = get_u64(&polling, "interval_ms")
            .unwrap_or(30_000);

        // -- workspace --
        let workspace_root = get_str(&workspace, "root")
            .map(|v| resolve_path(&v))
            .transpose()?
            .unwrap_or_else(|| {
                let mut p = env::temp_dir();
                p.push("symphony_workspaces");
                p
            });

        // -- hooks --
        let hooks = HooksConfig {
            after_create: get_str(&hooks_map, "after_create"),
            before_run: get_str(&hooks_map, "before_run"),
            after_run: get_str(&hooks_map, "after_run"),
            before_remove: get_str(&hooks_map, "before_remove"),
            timeout_ms: get_u64(&hooks_map, "timeout_ms").unwrap_or(60_000),
        };

        // -- agent --
        let agent_max_concurrent = get_u32(&agent, "max_concurrent_agents")
            .unwrap_or(10);
        let agent_max_turns = get_u32(&agent, "max_turns").unwrap_or(20);
        let agent_max_retry_backoff_ms = get_u64(&agent, "max_retry_backoff_ms")
            .unwrap_or(300_000);
        let agent_max_concurrent_by_state =
            get_str_u32_map(&agent, "max_concurrent_agents_by_state");

        // -- codex --
        let codex_command = get_str(&codex, "command")
            .unwrap_or_else(|| "codex app-server".to_owned());
        let codex_approval_policy = get_str(&codex, "approval_policy");
        let codex_thread_sandbox = get_str(&codex, "thread_sandbox");
        let codex_turn_sandbox_policy = get_str(&codex, "turn_sandbox_policy");
        let codex_turn_timeout_ms = get_u64(&codex, "turn_timeout_ms")
            .unwrap_or(3_600_000);
        let codex_read_timeout_ms = get_u64(&codex, "read_timeout_ms")
            .unwrap_or(5_000);
        let codex_stall_timeout_ms = get_i64(&codex, "stall_timeout_ms")
            .unwrap_or(300_000);

        // -- server --
        let server_port = get_u64(&server, "port").map(|v| v as u16);

        Ok(Self {
            tracker_kind,
            tracker_endpoint,
            tracker_api_key,
            tracker_project_slug,
            tracker_active_states,
            tracker_terminal_states,
            github_app_id,
            github_app_installation_id,
            github_app_private_key_path,
            polling_interval_ms,
            workspace_root,
            hooks,
            agent_max_concurrent,
            agent_max_turns,
            agent_max_retry_backoff_ms,
            agent_max_concurrent_by_state,
            codex_command,
            codex_approval_policy,
            codex_thread_sandbox,
            codex_turn_sandbox_policy,
            codex_turn_timeout_ms,
            codex_read_timeout_ms,
            codex_stall_timeout_ms,
            server_port,
        })
    }
}

// ---------------------------------------------------------------------------
// Helper functions for extracting typed values from serde_yaml_ng::Value
// ---------------------------------------------------------------------------

type Mapping = serde_yaml_ng::Mapping;

fn get_mapping(parent: &Mapping, key: &str) -> Mapping {
    parent
        .get(&serde_yaml_ng::Value::String(key.to_owned()))
        .and_then(|v| v.as_mapping())
        .cloned()
        .unwrap_or_default()
}

fn get_value<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a serde_yaml_ng::Value> {
    mapping.get(&serde_yaml_ng::Value::String(key.to_owned()))
}

fn get_str(mapping: &Mapping, key: &str) -> Option<String> {
    get_value(mapping, key).and_then(|v| match v {
        serde_yaml_ng::Value::String(s) => Some(s.clone()),
        serde_yaml_ng::Value::Number(n) => Some(n.to_string()),
        serde_yaml_ng::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    })
}

fn get_u64(mapping: &Mapping, key: &str) -> Option<u64> {
    get_value(mapping, key).and_then(|v| match v {
        serde_yaml_ng::Value::Number(n) => n.as_u64(),
        serde_yaml_ng::Value::String(s) => s.parse::<u64>().ok(),
        _ => None,
    })
}

fn get_u32(mapping: &Mapping, key: &str) -> Option<u32> {
    get_u64(mapping, key).map(|v| v as u32)
}

fn get_i64(mapping: &Mapping, key: &str) -> Option<i64> {
    get_value(mapping, key).and_then(|v| match v {
        serde_yaml_ng::Value::Number(n) => n.as_i64(),
        serde_yaml_ng::Value::String(s) => s.parse::<i64>().ok(),
        _ => None,
    })
}

fn get_string_list(mapping: &Mapping, key: &str) -> Option<Vec<String>> {
    get_value(mapping, key).and_then(|v| {
        v.as_sequence().map(|seq| {
            seq.iter()
                .filter_map(|item| item.as_str().map(String::from))
                .collect()
        })
    })
}

/// Extract a `HashMap<String, u32>` from a YAML mapping, normalizing keys to
/// lowercase.
fn get_str_u32_map(mapping: &Mapping, key: &str) -> HashMap<String, u32> {
    let mut result = HashMap::new();
    if let Some(inner) = get_value(mapping, key).and_then(|v| v.as_mapping()) {
        for (k, v) in inner {
            if let Some(key_str) = k.as_str() {
                let val = match v {
                    serde_yaml_ng::Value::Number(n) => n.as_u64().map(|n| n as u32),
                    serde_yaml_ng::Value::String(s) => s.parse::<u32>().ok(),
                    _ => None,
                };
                if let Some(val) = val {
                    result.insert(key_str.to_lowercase(), val);
                }
            }
        }
    }
    result
}

/// Resolve `$VAR` references in a string value to their environment variable
/// values. If the entire string starts with `$`, the remainder is treated as
/// an environment variable name. Returns an error if the variable is not set.
fn resolve_env_var(value: &str) -> Result<String> {
    if let Some(var_name) = value.strip_prefix('$') {
        env::var(var_name).map_err(|_| SymphonyError::ConfigValidation {
            detail: format!("environment variable ${var_name} is not set"),
        })
    } else {
        Ok(value.to_owned())
    }
}

/// Resolve path values: expand `~` to the user home directory and resolve
/// `$VAR` references.
fn resolve_path(value: &str) -> Result<PathBuf> {
    let resolved = resolve_env_var(value)?;
    let expanded = if let Some(rest) = resolved.strip_prefix('~') {
        let home = env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        let mut path = PathBuf::from(home);
        let suffix = rest.strip_prefix('/').unwrap_or(rest);
        if !suffix.is_empty() {
            path.push(suffix);
        }
        path
    } else {
        PathBuf::from(resolved)
    };
    Ok(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::workflow::WorkflowDefinition;

    fn make_workflow(yaml: &str) -> WorkflowDefinition {
        let config: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml).unwrap();
        WorkflowDefinition {
            config,
            prompt_template: String::new(),
        }
    }

    #[test]
    fn from_workflow_minimal() {
        // SAFETY: test-only; tests using env vars run serially via
        // `cargo test -- --test-threads=1` or accept the race.
        unsafe { env::set_var("TEST_API_KEY", "ghp_test123") };

        let wf = make_workflow(
            r#"
tracker:
  kind: github
  api_key: $TEST_API_KEY
  project_slug: owner/repo
"#,
        );

        let cfg = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(cfg.tracker_kind, "github");
        assert_eq!(cfg.tracker_api_key, "ghp_test123");
        assert_eq!(cfg.tracker_project_slug, "owner/repo");
        assert_eq!(cfg.tracker_endpoint, "https://api.github.com");
        assert_eq!(cfg.polling_interval_ms, 30_000);
        assert_eq!(cfg.agent_max_concurrent, 10);
        assert_eq!(cfg.codex_stall_timeout_ms, 300_000);
    }

    #[test]
    fn from_workflow_missing_api_key_defaults_to_empty() {
        let wf = make_workflow(
            r#"
tracker:
  kind: github
  project_slug: owner/repo
"#,
        );
        // api_key is now optional (can use GitHub App auth instead).
        let cfg = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(cfg.tracker_api_key, "");
    }

    #[test]
    fn from_workflow_missing_project_slug() {
        unsafe { env::set_var("TEST_API_KEY2", "key") };
        let wf = make_workflow(
            r#"
tracker:
  kind: github
  api_key: $TEST_API_KEY2
"#,
        );
        let err = ServiceConfig::from_workflow(&wf).unwrap_err();
        assert!(matches!(err, SymphonyError::MissingTrackerProjectSlug));
    }

    #[test]
    fn from_workflow_state_map_normalized() {
        unsafe { env::set_var("TEST_API_KEY3", "key") };
        let wf = make_workflow(
            r#"
tracker:
  kind: github
  api_key: $TEST_API_KEY3
  project_slug: owner/repo
agent:
  max_concurrent_agents_by_state:
    In Progress: 3
    Todo: 5
"#,
        );
        let cfg = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(
            cfg.agent_max_concurrent_by_state.get("in progress"),
            Some(&3)
        );
        assert_eq!(
            cfg.agent_max_concurrent_by_state.get("todo"),
            Some(&5)
        );
    }

    #[test]
    fn from_workflow_string_integer_coercion() {
        unsafe { env::set_var("TEST_API_KEY4", "key") };
        let wf = make_workflow(
            r#"
tracker:
  kind: github
  api_key: $TEST_API_KEY4
  project_slug: owner/repo
polling:
  interval_ms: "15000"
"#,
        );
        let cfg = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(cfg.polling_interval_ms, 15_000);
    }
}
