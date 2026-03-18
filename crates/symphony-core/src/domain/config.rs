use std::collections::HashMap;
use std::env;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Result, SymphonyError};

use super::issue::Issue;
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

/// The type of agent backend to use for a profile.
///
/// Determines the communication protocol: Codex uses JSON-RPC with
/// handshake and multi-turn loop; Claude CLI uses a single subprocess
/// invocation with streaming JSON output.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
    Codex,
    ClaudeCli,
}

impl Default for AgentType {
    fn default() -> Self {
        Self::Codex
    }
}

impl std::fmt::Display for AgentType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Codex => write!(f, "codex"),
            Self::ClaudeCli => write!(f, "claude-cli"),
        }
    }
}

/// Configuration for a single named agent profile.
///
/// Each profile describes how to launch and communicate with a specific
/// agent backend (e.g. Codex, Claude Code). Multiple profiles can coexist
/// in a single workflow, selected per-issue via labels.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentProfileConfig {
    pub agent_type: AgentType,
    pub command: String,
    pub approval_policy: Option<String>,
    pub thread_sandbox: Option<String>,
    pub turn_sandbox_policy: Option<String>,
    pub turn_timeout_ms: u64,
    pub read_timeout_ms: u64,
    pub stall_timeout_ms: i64,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub network_access: bool,
    pub max_turns: Option<u32>,
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
    /// If set, only issues with this label are dispatched.
    /// Prevents unauthorized issue creation from triggering agent runs.
    pub agent_require_label: Option<String>,

    // -- agent profiles (multi-agent support) --
    pub agent_profiles: HashMap<String, AgentProfileConfig>,
    pub default_agent: String,

    // -- codex (backward-compatible fields, populated from default profile) --
    pub codex_command: String,
    pub codex_approval_policy: Option<String>,
    pub codex_thread_sandbox: Option<String>,
    pub codex_turn_sandbox_policy: Option<String>,
    pub codex_turn_timeout_ms: u64,
    pub codex_read_timeout_ms: u64,
    pub codex_stall_timeout_ms: i64,
    pub codex_model: Option<String>,
    pub codex_reasoning_effort: Option<String>,
    pub codex_network_access: bool,
    pub codex_auto_merge: bool,

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

        // -- github app (all fields support $VAR env references) --
        let github_app_id = get_str(&tracker, "app_id")
            .map(|v| resolve_env_var(&v))
            .transpose()?
            .and_then(|v| v.parse::<u64>().ok());
        let github_app_installation_id = get_str(&tracker, "installation_id")
            .map(|v| resolve_env_var(&v))
            .transpose()?
            .and_then(|v| v.parse::<u64>().ok());
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
        let agent_require_label = get_str(&agent, "require_label");
        let agent_max_concurrent = get_u32(&agent, "max_concurrent_agents")
            .unwrap_or(10);
        let agent_max_turns = get_u32(&agent, "max_turns").unwrap_or(20);
        let agent_max_retry_backoff_ms = get_u64(&agent, "max_retry_backoff_ms")
            .unwrap_or(300_000);
        let agent_max_concurrent_by_state =
            get_str_u32_map(&agent, "max_concurrent_agents_by_state");
        let codex_auto_merge = get_str(&agent, "auto_merge")
            .or_else(|| get_str(&codex, "auto_merge"))
            .map(|v| v.to_lowercase() == "true")
            .unwrap_or(false);

        // -- agent profiles (multi-agent) --
        let (agent_profiles, default_agent) =
            parse_agent_profiles(root, &agent, &codex)?;

        // Populate backward-compatible codex_* fields from the default profile.
        let default_profile = agent_profiles
            .get(&default_agent)
            .or_else(|| agent_profiles.values().next())
            .cloned()
            .unwrap_or_else(default_agent_profile);

        let codex_command = default_profile.command.clone();
        let codex_approval_policy = default_profile.approval_policy.clone();
        let codex_thread_sandbox = default_profile.thread_sandbox.clone();
        let codex_turn_sandbox_policy = default_profile.turn_sandbox_policy.clone();
        let codex_turn_timeout_ms = default_profile.turn_timeout_ms;
        let codex_read_timeout_ms = default_profile.read_timeout_ms;
        let codex_stall_timeout_ms = default_profile.stall_timeout_ms;
        let codex_model = default_profile.model.clone();
        let codex_reasoning_effort = default_profile.reasoning_effort.clone();
        let codex_network_access = default_profile.network_access;

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
            agent_require_label,
            agent_profiles,
            default_agent,
            codex_command,
            codex_approval_policy,
            codex_thread_sandbox,
            codex_turn_sandbox_policy,
            codex_turn_timeout_ms,
            codex_read_timeout_ms,
            codex_stall_timeout_ms,
            codex_model,
            codex_reasoning_effort,
            codex_network_access,
            codex_auto_merge,
            server_port,
        })
    }

    /// Look up an agent profile by name.
    pub fn get_agent_profile(&self, name: &str) -> Option<&AgentProfileConfig> {
        self.agent_profiles.get(name)
    }

    /// Resolve which agent profile to use for a given issue.
    ///
    /// Checks for a label prefixed with `agent:` (e.g. `agent:claude`) and
    /// returns the matching profile. Falls back to the configured default.
    pub fn resolve_agent_for_issue(&self, issue: &Issue) -> &AgentProfileConfig {
        for label in &issue.labels {
            if let Some(agent_name) = label.strip_prefix("agent:") {
                if let Some(profile) = self.agent_profiles.get(agent_name) {
                    return profile;
                }
            }
        }
        self.agent_profiles
            .get(&self.default_agent)
            .or_else(|| self.agent_profiles.values().next())
            .expect("at least one agent profile must be configured")
    }
}

/// Build a default `AgentProfileConfig` with standard Codex defaults.
fn default_agent_profile() -> AgentProfileConfig {
    AgentProfileConfig {
        agent_type: AgentType::Codex,
        command: "codex app-server".to_owned(),
        approval_policy: None,
        thread_sandbox: None,
        turn_sandbox_policy: None,
        turn_timeout_ms: 3_600_000,
        read_timeout_ms: 5_000,
        stall_timeout_ms: 300_000,
        model: None,
        reasoning_effort: None,
        network_access: true,
        max_turns: None,
    }
}

/// Parse an `AgentProfileConfig` from a YAML mapping section.
fn parse_profile_from_mapping(mapping: &Mapping) -> AgentProfileConfig {
    let agent_type = get_str(mapping, "agent_type")
        .or_else(|| get_str(mapping, "type"))
        .map(|v| match v.to_lowercase().as_str() {
            "claude-cli" | "claude_cli" | "claudecli" => AgentType::ClaudeCli,
            _ => AgentType::Codex,
        })
        .unwrap_or(AgentType::Codex);

    AgentProfileConfig {
        agent_type,
        command: get_str(mapping, "command")
            .unwrap_or_else(|| "codex app-server".to_owned()),
        approval_policy: get_str(mapping, "approval_policy"),
        thread_sandbox: get_str(mapping, "thread_sandbox"),
        turn_sandbox_policy: get_str(mapping, "turn_sandbox_policy"),
        turn_timeout_ms: get_u64(mapping, "turn_timeout_ms")
            .unwrap_or(3_600_000),
        read_timeout_ms: get_u64(mapping, "read_timeout_ms")
            .unwrap_or(5_000),
        stall_timeout_ms: get_i64(mapping, "stall_timeout_ms")
            .unwrap_or(300_000),
        model: get_str(mapping, "model"),
        reasoning_effort: get_str(mapping, "reasoning_effort"),
        network_access: get_str(mapping, "network_access")
            .map(|v| v.to_lowercase() != "false")
            .unwrap_or(true),
        max_turns: get_u32(mapping, "max_turns"),
    }
}

/// Parse the `agents:` map (new multi-agent format) or fall back to the
/// legacy `codex:` section for backward compatibility.
///
/// Returns `(profiles_map, default_agent_name)`.
fn parse_agent_profiles(
    root: &Mapping,
    agent: &Mapping,
    codex: &Mapping,
) -> Result<(HashMap<String, AgentProfileConfig>, String)> {
    let agents_mapping = get_value(root, "agents").and_then(|v| v.as_mapping());

    if let Some(agents) = agents_mapping {
        // New format: `agents:` map with named entries.
        let mut profiles = HashMap::new();
        for (key, value) in agents {
            if let Some(name) = key.as_str() {
                let profile_map = value.as_mapping().cloned().unwrap_or_default();
                profiles.insert(
                    name.to_owned(),
                    parse_profile_from_mapping(&profile_map),
                );
            }
        }
        if profiles.is_empty() {
            profiles.insert("codex".to_owned(), default_agent_profile());
        }
        let default_name = get_str(agent, "default")
            .unwrap_or_else(|| {
                profiles
                    .keys()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "codex".to_owned())
            });
        Ok((profiles, default_name))
    } else {
        // Legacy format: single `codex:` section becomes one profile.
        let profile = parse_profile_from_mapping(codex);
        let mut profiles = HashMap::new();
        profiles.insert("codex".to_owned(), profile);
        Ok((profiles, "codex".to_owned()))
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
        // Should create a default "codex" profile.
        assert_eq!(cfg.default_agent, "codex");
        assert!(cfg.agent_profiles.contains_key("codex"));
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

    #[test]
    fn from_workflow_legacy_codex_creates_single_profile() {
        unsafe { env::set_var("TEST_API_KEY5", "key") };
        let wf = make_workflow(
            r#"
tracker:
  kind: github
  api_key: $TEST_API_KEY5
  project_slug: owner/repo
codex:
  command: codex app-server
  model: gpt-5.3-codex
  stall_timeout_ms: 600000
"#,
        );
        let cfg = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(cfg.default_agent, "codex");
        assert_eq!(cfg.agent_profiles.len(), 1);

        let profile = cfg.agent_profiles.get("codex").unwrap();
        assert_eq!(profile.command, "codex app-server");
        assert_eq!(profile.model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(profile.stall_timeout_ms, 600_000);

        // Backward-compat fields should match.
        assert_eq!(cfg.codex_command, "codex app-server");
        assert_eq!(cfg.codex_model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(cfg.codex_stall_timeout_ms, 600_000);
    }

    #[test]
    fn from_workflow_multi_agent_profiles() {
        unsafe { env::set_var("TEST_API_KEY6", "key") };
        let wf = make_workflow(
            r#"
tracker:
  kind: github
  api_key: $TEST_API_KEY6
  project_slug: owner/repo
agents:
  codex:
    command: codex app-server
    model: gpt-5.3-codex
    stall_timeout_ms: 600000
  claude:
    command: claude-app-server
    model: claude-sonnet-4-6
    reasoning_effort: high
    network_access: false
agent:
  default: codex
"#,
        );
        let cfg = ServiceConfig::from_workflow(&wf).unwrap();
        assert_eq!(cfg.agent_profiles.len(), 2);
        assert_eq!(cfg.default_agent, "codex");

        let codex = cfg.agent_profiles.get("codex").unwrap();
        assert_eq!(codex.command, "codex app-server");
        assert_eq!(codex.model.as_deref(), Some("gpt-5.3-codex"));
        assert_eq!(codex.stall_timeout_ms, 600_000);

        let claude = cfg.agent_profiles.get("claude").unwrap();
        assert_eq!(claude.command, "claude-app-server");
        assert_eq!(claude.model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(claude.reasoning_effort.as_deref(), Some("high"));
        assert!(!claude.network_access);

        // Backward-compat fields should come from the default profile.
        assert_eq!(cfg.codex_command, "codex app-server");
        assert_eq!(cfg.codex_stall_timeout_ms, 600_000);
    }

    #[test]
    fn resolve_agent_for_issue_default() {
        unsafe { env::set_var("TEST_API_KEY7", "key") };
        let wf = make_workflow(
            r#"
tracker:
  kind: github
  api_key: $TEST_API_KEY7
  project_slug: owner/repo
agents:
  codex:
    command: codex app-server
  claude:
    command: claude-app-server
agent:
  default: codex
"#,
        );
        let cfg = ServiceConfig::from_workflow(&wf).unwrap();

        let issue = Issue {
            id: "1".to_string(),
            identifier: "#1".to_string(),
            title: "Test".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            labels: vec![],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        };

        let profile = cfg.resolve_agent_for_issue(&issue);
        assert_eq!(profile.command, "codex app-server");
    }

    #[test]
    fn resolve_agent_for_issue_by_label() {
        unsafe { env::set_var("TEST_API_KEY8", "key") };
        let wf = make_workflow(
            r#"
tracker:
  kind: github
  api_key: $TEST_API_KEY8
  project_slug: owner/repo
agents:
  codex:
    command: codex app-server
  claude:
    command: claude-app-server
agent:
  default: codex
"#,
        );
        let cfg = ServiceConfig::from_workflow(&wf).unwrap();

        let issue = Issue {
            id: "2".to_string(),
            identifier: "#2".to_string(),
            title: "Test".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            labels: vec!["agent:claude".to_string()],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        };

        let profile = cfg.resolve_agent_for_issue(&issue);
        assert_eq!(profile.command, "claude-app-server");
    }

    #[test]
    fn resolve_agent_for_issue_unknown_label_falls_back() {
        unsafe { env::set_var("TEST_API_KEY9", "key") };
        let wf = make_workflow(
            r#"
tracker:
  kind: github
  api_key: $TEST_API_KEY9
  project_slug: owner/repo
agents:
  codex:
    command: codex app-server
agent:
  default: codex
"#,
        );
        let cfg = ServiceConfig::from_workflow(&wf).unwrap();

        let issue = Issue {
            id: "3".to_string(),
            identifier: "#3".to_string(),
            title: "Test".to_string(),
            description: None,
            priority: None,
            state: "Todo".to_string(),
            branch_name: None,
            url: None,
            labels: vec!["agent:nonexistent".to_string()],
            blocked_by: vec![],
            created_at: None,
            updated_at: None,
        };

        let profile = cfg.resolve_agent_for_issue(&issue);
        assert_eq!(profile.command, "codex app-server");
    }
}
