//! Dispatch preflight validation per Section 6.3 of the Symphony spec.
//!
//! Validates that all required configuration fields are present and
//! well-formed before dispatching work to agents.

use std::collections::{HashMap, HashSet};

use symphony_core::domain::{AgentType, PipelineStage, ServiceConfig};
use symphony_core::error::SymphonyError;
use symphony_core::identifiers::normalize_state;

/// Supported tracker kinds.
const SUPPORTED_TRACKER_KINDS: &[&str] = &["github"];

/// Validate that a `ServiceConfig` has all required fields for dispatching.
///
/// # Checks performed
///
/// - `tracker_kind` is a supported value ("github").
/// - `tracker_api_key` is non-empty.
/// - `tracker_project_slug` is non-empty.
/// - `codex_command` is non-empty.
///
/// # Errors
///
/// Returns `ConfigValidation` with a descriptive message on the first
/// failing check.
pub fn validate_dispatch_config(config: &ServiceConfig) -> Result<(), SymphonyError> {
    for check in validation_checks(config) {
        check?;
    }
    Ok(())
}

/// Collect all validation errors instead of failing on the first one.
///
/// Returns a `Vec` of error messages. An empty vec means validation passed.
pub fn validate_all(config: &ServiceConfig) -> Vec<String> {
    validation_checks(config)
        .into_iter()
        .filter_map(|r| r.err())
        .map(|e| e.to_string())
        .collect()
}

fn validation_checks(config: &ServiceConfig) -> Vec<Result<(), SymphonyError>> {
    let mut checks = vec![
        validate_tracker_kind(config),
        validate_tracker_api_key(config),
        validate_tracker_project_slug(config),
        validate_codex_command(config),
        validate_default_agent(config),
        validate_agent_profiles(config),
        validate_turn_limits(config),
        validate_tracker_states(config),
    ];
    checks.extend(validate_pipeline_stages(config));
    checks
}

fn validate_tracker_kind(config: &ServiceConfig) -> Result<(), SymphonyError> {
    if config.tracker_kind.trim().is_empty() {
        return Err(SymphonyError::ConfigValidation {
            detail: "tracker_kind is required".to_string(),
        });
    }

    if !SUPPORTED_TRACKER_KINDS.contains(&config.tracker_kind.as_str()) {
        return Err(SymphonyError::ConfigValidation {
            detail: format!(
                "tracker_kind '{}' is not supported; supported: {SUPPORTED_TRACKER_KINDS:?}",
                config.tracker_kind
            ),
        });
    }

    Ok(())
}

fn validate_tracker_api_key(config: &ServiceConfig) -> Result<(), SymphonyError> {
    let has_app_auth = config.github_app_id.is_some()
        && config.github_app_installation_id.is_some()
        && config.github_app_private_key_path.is_some();

    if config.tracker_api_key.trim().is_empty() && !has_app_auth {
        return Err(SymphonyError::ConfigValidation {
            detail: "tracker_api_key or GitHub App config (app_id + installation_id + private_key_path) is required".to_string(),
        });
    }
    Ok(())
}

fn validate_tracker_project_slug(config: &ServiceConfig) -> Result<(), SymphonyError> {
    if config.tracker_project_slug.trim().is_empty() {
        return Err(SymphonyError::ConfigValidation {
            detail: "tracker_project_slug is required and must not be empty".to_string(),
        });
    }
    Ok(())
}

fn validate_codex_command(config: &ServiceConfig) -> Result<(), SymphonyError> {
    // Check that the default agent profile has a non-empty command.
    // Also check the legacy codex_command for backward compatibility.
    if config.codex_command.trim().is_empty() {
        return Err(SymphonyError::ConfigValidation {
            detail: "codex_command is required and must not be empty".to_string(),
        });
    }
    Ok(())
}

fn validate_default_agent(config: &ServiceConfig) -> Result<(), SymphonyError> {
    if !config.agent_profiles.contains_key(&config.default_agent) {
        return Err(SymphonyError::ConfigValidation {
            detail: format!(
                "agent.default '{}' does not match any configured agent profile",
                config.default_agent
            ),
        });
    }
    Ok(())
}

fn validate_agent_profiles(config: &ServiceConfig) -> Result<(), SymphonyError> {
    if config.agent_profiles.is_empty() {
        return Err(SymphonyError::ConfigValidation {
            detail: "at least one agent profile must be configured".to_string(),
        });
    }

    for (name, profile) in &config.agent_profiles {
        if profile.command.trim().is_empty() {
            return Err(SymphonyError::ConfigValidation {
                detail: format!("agent profile '{name}' must have a non-empty command"),
            });
        }

        if profile.stall_timeout_ms < 0 {
            return Err(SymphonyError::ConfigValidation {
                detail: format!(
                    "agent profile '{name}' has invalid stall_timeout_ms {}; use 0 to disable or a positive value",
                    profile.stall_timeout_ms
                ),
            });
        }

        if let Some(max_turns) = profile.max_turns {
            if max_turns == 0 {
                return Err(SymphonyError::ConfigValidation {
                    detail: format!("agent profile '{name}' must have max_turns > 0"),
                });
            }
        }
    }

    for (state, agent_name) in &config.agent_by_state {
        if !config.agent_profiles.contains_key(agent_name) {
            return Err(SymphonyError::ConfigValidation {
                detail: format!(
                    "agent.by_state maps state '{state}' to unknown profile '{agent_name}'"
                ),
            });
        }
    }

    Ok(())
}

fn validate_turn_limits(config: &ServiceConfig) -> Result<(), SymphonyError> {
    if config.agent_max_turns == 0 {
        return Err(SymphonyError::ConfigValidation {
            detail: "agent.max_turns must be greater than 0".to_string(),
        });
    }
    Ok(())
}

fn validate_tracker_states(config: &ServiceConfig) -> Result<(), SymphonyError> {
    let active: HashSet<String> = config
        .tracker_active_states
        .iter()
        .map(|state| normalize_state(state))
        .collect();
    let terminal: HashSet<String> = config
        .tracker_terminal_states
        .iter()
        .map(|state| normalize_state(state))
        .collect();

    if let Some(shared) = active.intersection(&terminal).next() {
        return Err(SymphonyError::ConfigValidation {
            detail: format!(
                "tracker.active_states and tracker.terminal_states overlap on '{shared}'"
            ),
        });
    }

    Ok(())
}

fn validate_pipeline_stages(config: &ServiceConfig) -> Vec<Result<(), SymphonyError>> {
    let mut errors = Vec::new();
    let known_states: HashSet<String> = config
        .tracker_active_states
        .iter()
        .chain(config.tracker_terminal_states.iter())
        .chain(config.pipeline_stages.iter().map(|stage| &stage.state))
        .map(|state| normalize_state(state))
        .collect();
    let mut seen_state_roles = HashSet::new();

    for stage in &config.pipeline_stages {
        let role = stage
            .role
            .as_deref()
            .unwrap_or(&stage.agent)
            .trim()
            .to_string();
        let state = normalize_state(&stage.state);
        let state_role_key = format!("{state}:{role}");

        if role.is_empty() {
            errors.push(Err(SymphonyError::ConfigValidation {
                detail: format!(
                    "pipeline stage for state '{}' must have a non-empty role",
                    stage.state
                ),
            }));
        } else if role.contains(':') {
            errors.push(Err(SymphonyError::ConfigValidation {
                detail: format!(
                    "pipeline role '{}' is invalid; ':' is reserved for Symphony stage keys",
                    role
                ),
            }));
        }

        if !seen_state_roles.insert(state_role_key) {
            errors.push(Err(SymphonyError::ConfigValidation {
                detail: format!(
                    "pipeline state '{}' defines duplicate role '{}'; roles must be unique per state",
                    stage.state, role
                ),
            }));
        }

        if stage.agent == "none" {
            if stage.transition_to.is_some() || stage.reject_to.is_some() {
                errors.push(Err(SymphonyError::ConfigValidation {
                    detail: format!(
                        "pipeline stage '{}' uses agent: none and cannot define transitions",
                        stage.state
                    ),
                }));
            }
        } else if !config.agent_profiles.contains_key(&stage.agent) {
            errors.push(Err(SymphonyError::ConfigValidation {
                detail: format!(
                    "pipeline stage '{}' references unknown agent profile '{}'",
                    stage.state, stage.agent
                ),
            }));
        }

        if stage.reject_to.is_some() && stage.transition_to.is_none() {
            errors.push(Err(SymphonyError::ConfigValidation {
                detail: format!(
                    "pipeline stage '{}' defines reject_to without transition_to",
                    stage.state
                ),
            }));
        }

        validate_stage_target(
            stage,
            &stage.transition_to,
            "transition_to",
            &known_states,
            &mut errors,
        );
        validate_stage_target(
            stage,
            &stage.reject_to,
            "reject_to",
            &known_states,
            &mut errors,
        );
    }

    validate_parallel_stage_groups(config, &mut errors);

    errors
}

fn validate_parallel_stage_groups(
    config: &ServiceConfig,
    errors: &mut Vec<Result<(), SymphonyError>>,
) {
    let mut groups: HashMap<String, Vec<&PipelineStage>> = HashMap::new();
    for stage in &config.pipeline_stages {
        if stage.agent == "none" {
            continue;
        }
        groups
            .entry(normalize_state(&stage.state))
            .or_default()
            .push(stage);
    }

    for (state, stages) in groups {
        if stages.len() <= 1 {
            continue;
        }

        let mut seen_scopes: HashMap<String, String> = HashMap::new();
        let mut expected_transition: Option<Option<String>> = None;
        let mut expected_reject: Option<Option<String>> = None;

        for stage in stages {
            let role = stage.role.as_deref().unwrap_or(&stage.agent).to_string();
            let scope = normalize_parallel_scope(stage.scope.as_deref());
            let display_state = stage.state.clone();

            let Some(scope) = scope else {
                errors.push(Err(SymphonyError::ConfigValidation {
                    detail: format!(
                        "parallel pipeline state '{}' requires every runnable stage to define a non-root scope; role '{}' is missing one",
                        display_state, role
                    ),
                }));
                continue;
            };

            if let Some(existing_role) = seen_scopes.insert(scope.clone(), role.clone()) {
                errors.push(Err(SymphonyError::ConfigValidation {
                    detail: format!(
                        "parallel pipeline state '{}' defines duplicate scope '{}' for roles '{}' and '{}'",
                        display_state, scope, existing_role, role
                    ),
                }));
            }

            let transition = stage.transition_to.as_ref().map(|v| normalize_state(v));
            match &expected_transition {
                None => expected_transition = Some(transition.clone()),
                Some(expected) if *expected != transition => {
                    errors.push(Err(SymphonyError::ConfigValidation {
                        detail: format!(
                            "parallel pipeline state '{}' must use a single transition_to target across all runnable stages",
                            display_state
                        ),
                    }));
                }
                _ => {}
            }

            let reject = stage.reject_to.as_ref().map(|v| normalize_state(v));
            match &expected_reject {
                None => expected_reject = Some(reject.clone()),
                Some(expected) if *expected != reject => {
                    errors.push(Err(SymphonyError::ConfigValidation {
                        detail: format!(
                            "parallel pipeline state '{}' must use a single reject_to target across all runnable stages",
                            display_state
                        ),
                    }));
                }
                _ => {}
            }

            validate_parallel_stage_agent_coordination(
                stage,
                config,
                &display_state,
                &role,
                errors,
            );
        }

        let _ = state;
    }
}

fn validate_parallel_stage_agent_coordination(
    stage: &PipelineStage,
    config: &ServiceConfig,
    display_state: &str,
    role: &str,
    errors: &mut Vec<Result<(), SymphonyError>>,
) {
    let Some(profile) = config.agent_profiles.get(&stage.agent) else {
        return;
    };

    if profile.agent_type != AgentType::ClaudeCli || profile.allowed_tools.is_empty() {
        return;
    }

    if claude_allowlist_supports_coordination_helpers(&profile.allowed_tools) {
        return;
    }

    errors.push(Err(SymphonyError::ConfigValidation {
        detail: format!(
            "parallel pipeline state '{}' role '{}' uses Claude with a strict allowed_tools list but does not allow Symphony coordination helpers; include Bash access for symphony-note, symphony-mailbox, and symphony-claim or remove the allowlist",
            display_state, role
        ),
    }));
}

fn normalize_parallel_scope(scope: Option<&str>) -> Option<String> {
    let scope = scope?.trim();
    if scope.is_empty() || scope == "." || scope == "./" || scope == "/" {
        return None;
    }
    Some(scope.trim_matches('/').to_string())
}

fn claude_allowlist_supports_coordination_helpers(allowed_tools: &[String]) -> bool {
    if allowed_tools
        .iter()
        .any(|tool| tool.trim().eq_ignore_ascii_case("bash"))
    {
        return true;
    }

    let mut note = false;
    let mut mailbox = false;
    let mut claim = false;

    for tool in allowed_tools {
        let normalized = tool.trim().to_lowercase();
        if normalized.contains("symphony-note") {
            note = true;
        }
        if normalized.contains("symphony-mailbox") {
            mailbox = true;
        }
        if normalized.contains("symphony-claim") {
            claim = true;
        }
    }

    note && mailbox && claim
}

fn validate_stage_target(
    stage: &PipelineStage,
    target: &Option<String>,
    field_name: &str,
    known_states: &HashSet<String>,
    errors: &mut Vec<Result<(), SymphonyError>>,
) {
    let Some(target_state) = target.as_deref() else {
        return;
    };

    let normalized_target = normalize_state(target_state);
    if normalized_target == normalize_state(&stage.state) {
        errors.push(Err(SymphonyError::ConfigValidation {
            detail: format!(
                "pipeline stage '{}' has {field_name} pointing back to the same state",
                stage.state
            ),
        }));
    }

    if !known_states.contains(&normalized_target) {
        errors.push(Err(SymphonyError::ConfigValidation {
            detail: format!(
                "pipeline stage '{}' has unknown {field_name} target '{}'",
                stage.state, target_state
            ),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use symphony_core::domain::config::{AgentProfileConfig, AgentType, HooksConfig};

    fn valid_config() -> ServiceConfig {
        let default_profile = AgentProfileConfig {
            agent_type: AgentType::Codex,
            command: "codex".to_string(),
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
            allowed_tools: vec![],
            disallowed_tools: vec![],
        };
        let mut agent_profiles = HashMap::new();
        agent_profiles.insert("codex".to_string(), default_profile);

        ServiceConfig {
            tracker_kind: "github".to_string(),
            tracker_endpoint: "https://api.github.com".to_string(),
            tracker_api_key: "ghp_test_token_123".to_string(),
            tracker_project_slug: "owner/repo".to_string(),
            tracker_active_states: vec!["Todo".to_string()],
            tracker_terminal_states: vec!["Done".to_string()],
            polling_interval_ms: 30_000,
            workspace_root: PathBuf::from("/tmp/workspaces"),
            git_user_name: None,
            git_user_email: None,
            hooks: HooksConfig {
                after_create: None,
                before_run: None,
                after_run: None,
                before_remove: None,
                timeout_ms: 60_000,
            },
            agent_max_concurrent: 10,
            agent_max_turns: 20,
            agent_max_retry_backoff_ms: 300_000,
            agent_max_concurrent_by_state: HashMap::new(),
            agent_require_label: None,
            agent_by_state: HashMap::new(),
            agent_profiles,
            default_agent: "codex".to_string(),
            codex_command: "codex".to_string(),
            codex_approval_policy: None,
            codex_thread_sandbox: None,
            codex_turn_sandbox_policy: None,
            codex_turn_timeout_ms: 3_600_000,
            codex_read_timeout_ms: 5_000,
            codex_stall_timeout_ms: 300_000,
            server_port: None,
            github_app_id: None,
            github_app_installation_id: None,
            github_app_private_key_path: None,
            codex_model: None,
            codex_reasoning_effort: None,
            codex_network_access: true,
            codex_auto_merge: false,
            pipeline_stages: vec![],
            prompt_state_instructions: HashMap::new(),
            prompt_role_instructions: HashMap::new(),
        }
    }

    #[test]
    fn valid_config_passes() {
        assert!(validate_dispatch_config(&valid_config()).is_ok());
    }

    #[test]
    fn empty_tracker_kind_fails() {
        let mut cfg = valid_config();
        cfg.tracker_kind = String::new();
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("tracker_kind"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn unsupported_tracker_kind() {
        let mut cfg = valid_config();
        cfg.tracker_kind = "jira".to_string();
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("not supported"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn empty_tracker_api_key() {
        let mut cfg = valid_config();
        cfg.tracker_api_key = String::new();
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("tracker_api_key"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn whitespace_only_api_key() {
        let mut cfg = valid_config();
        cfg.tracker_api_key = "  ".to_string();
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("tracker_api_key"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn empty_project_slug() {
        let mut cfg = valid_config();
        cfg.tracker_project_slug = "  ".to_string();
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("tracker_project_slug"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn empty_codex_command() {
        let mut cfg = valid_config();
        cfg.codex_command = String::new();
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("codex_command"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn zero_max_turns_fails() {
        let mut cfg = valid_config();
        cfg.agent_max_turns = 0;
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("max_turns"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn overlapping_active_and_terminal_states_fail() {
        let mut cfg = valid_config();
        cfg.tracker_terminal_states.push("todo".to_string());
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("overlap"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn pipeline_unknown_agent_fails() {
        let mut cfg = valid_config();
        cfg.pipeline_stages.push(PipelineStage {
            state: "in progress".to_string(),
            agent: "claude".to_string(),
            role: Some("reviewer".to_string()),
            prompt: None,
            transition_to: Some("done".to_string()),
            reject_to: None,
            when_labels: vec![],
            scope: None,
        });
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("unknown agent profile"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn pipeline_duplicate_role_fails() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("In Progress".to_string());
        cfg.pipeline_stages = vec![
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("implementer".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec![],
                scope: None,
            },
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("implementer".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec!["backend".to_string()],
                scope: None,
            },
        ];
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("duplicate role"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn pipeline_none_agent_with_transition_fails() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("Human Review".to_string());
        cfg.pipeline_stages.push(PipelineStage {
            state: "human review".to_string(),
            agent: "none".to_string(),
            role: Some("handoff".to_string()),
            prompt: None,
            transition_to: Some("done".to_string()),
            reject_to: None,
            when_labels: vec![],
            scope: None,
        });
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("agent: none"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn pipeline_reject_without_transition_fails() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("Code Review".to_string());
        cfg.tracker_active_states.push("Rework".to_string());
        cfg.pipeline_stages.push(PipelineStage {
            state: "code review".to_string(),
            agent: "codex".to_string(),
            role: Some("reviewer".to_string()),
            prompt: None,
            transition_to: None,
            reject_to: Some("rework".to_string()),
            when_labels: vec![],
            scope: None,
        });
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("reject_to without transition_to"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn pipeline_self_loop_fails() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("Code Review".to_string());
        cfg.pipeline_stages.push(PipelineStage {
            state: "code review".to_string(),
            agent: "codex".to_string(),
            role: Some("reviewer".to_string()),
            prompt: None,
            transition_to: Some("code-review".to_string()),
            reject_to: None,
            when_labels: vec![],
            scope: None,
        });
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("transition_to"), "{detail}");
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn pipeline_role_with_colon_fails() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("In Progress".to_string());
        cfg.pipeline_stages.push(PipelineStage {
            state: "in progress".to_string(),
            agent: "codex".to_string(),
            role: Some("backend:implementer".to_string()),
            prompt: None,
            transition_to: Some("done".to_string()),
            reject_to: None,
            when_labels: vec![],
            scope: None,
        });
        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("reserved"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn parallel_pipeline_stage_requires_scope() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("In Progress".to_string());
        cfg.pipeline_stages = vec![
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("backend-dev".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec!["backend".to_string()],
                scope: None,
            },
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("frontend-dev".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec!["frontend".to_string()],
                scope: Some("frontend/".to_string()),
            },
        ];

        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("non-root scope"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn parallel_pipeline_stage_rejects_duplicate_scope() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("In Progress".to_string());
        cfg.pipeline_stages = vec![
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("backend-dev".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec!["backend".to_string()],
                scope: Some("backend/".to_string()),
            },
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("api-dev".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec!["api".to_string()],
                scope: Some("backend".to_string()),
            },
        ];

        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("duplicate scope"));
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn parallel_pipeline_stage_requires_consistent_transition_targets() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("in progress".to_string());
        cfg.tracker_active_states.push("code-review".to_string());
        cfg.tracker_active_states.push("human-review".to_string());
        cfg.pipeline_stages = vec![
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("backend-dev".to_string()),
                prompt: None,
                transition_to: Some("code-review".to_string()),
                reject_to: None,
                when_labels: vec!["backend".to_string()],
                scope: Some("backend/".to_string()),
            },
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("frontend-dev".to_string()),
                prompt: None,
                transition_to: Some("human-review".to_string()),
                reject_to: None,
                when_labels: vec!["frontend".to_string()],
                scope: Some("frontend/".to_string()),
            },
        ];

        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("single transition_to target"), "{detail}");
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn parallel_pipeline_stage_requires_consistent_reject_targets() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("code review".to_string());
        cfg.tracker_active_states.push("human-review".to_string());
        cfg.tracker_active_states.push("rework".to_string());
        cfg.pipeline_stages = vec![
            PipelineStage {
                state: "code review".to_string(),
                agent: "codex".to_string(),
                role: Some("security-review".to_string()),
                prompt: None,
                transition_to: Some("human-review".to_string()),
                reject_to: Some("rework".to_string()),
                when_labels: vec!["security".to_string()],
                scope: Some("security/".to_string()),
            },
            PipelineStage {
                state: "code review".to_string(),
                agent: "codex".to_string(),
                role: Some("api-review".to_string()),
                prompt: None,
                transition_to: Some("human-review".to_string()),
                reject_to: Some("todo".to_string()),
                when_labels: vec!["api".to_string()],
                scope: Some("api/".to_string()),
            },
        ];

        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("single reject_to target"), "{detail}");
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn parallel_claude_stage_requires_coordination_helpers_in_allowlist() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("in progress".to_string());
        cfg.agent_profiles.insert(
            "claude".to_string(),
            AgentProfileConfig {
                agent_type: AgentType::ClaudeCli,
                command: "claude".to_string(),
                approval_policy: Some("never".to_string()),
                thread_sandbox: None,
                turn_sandbox_policy: None,
                turn_timeout_ms: 7_200_000,
                read_timeout_ms: 30_000,
                stall_timeout_ms: 600_000,
                model: None,
                reasoning_effort: None,
                network_access: true,
                max_turns: Some(20),
                allowed_tools: vec!["Read".to_string(), "Bash(gh pr:*)".to_string()],
                disallowed_tools: vec!["Edit".to_string()],
            },
        );
        cfg.pipeline_stages = vec![
            PipelineStage {
                state: "in progress".to_string(),
                agent: "claude".to_string(),
                role: Some("backend-review".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec!["backend".to_string()],
                scope: Some("backend/".to_string()),
            },
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("frontend-implementer".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec!["frontend".to_string()],
                scope: Some("frontend/".to_string()),
            },
        ];

        let err = validate_dispatch_config(&cfg).unwrap_err();
        match err {
            SymphonyError::ConfigValidation { detail } => {
                assert!(detail.contains("coordination helpers"), "{detail}");
            }
            other => panic!("expected ConfigValidation, got: {other:?}"),
        }
    }

    #[test]
    fn parallel_claude_stage_accepts_coordination_helpers_in_allowlist() {
        let mut cfg = valid_config();
        cfg.tracker_active_states.push("in progress".to_string());
        cfg.agent_profiles.insert(
            "claude".to_string(),
            AgentProfileConfig {
                agent_type: AgentType::ClaudeCli,
                command: "claude".to_string(),
                approval_policy: Some("never".to_string()),
                thread_sandbox: None,
                turn_sandbox_policy: None,
                turn_timeout_ms: 7_200_000,
                read_timeout_ms: 30_000,
                stall_timeout_ms: 600_000,
                model: None,
                reasoning_effort: None,
                network_access: true,
                max_turns: Some(20),
                allowed_tools: vec![
                    "Read".to_string(),
                    "Bash(symphony-note:*)".to_string(),
                    "Bash(symphony-mailbox:*)".to_string(),
                    "Bash(symphony-claim:*)".to_string(),
                ],
                disallowed_tools: vec!["Edit".to_string()],
            },
        );
        cfg.pipeline_stages = vec![
            PipelineStage {
                state: "in progress".to_string(),
                agent: "claude".to_string(),
                role: Some("backend-review".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec!["backend".to_string()],
                scope: Some("backend/".to_string()),
            },
            PipelineStage {
                state: "in progress".to_string(),
                agent: "codex".to_string(),
                role: Some("frontend-implementer".to_string()),
                prompt: None,
                transition_to: Some("done".to_string()),
                reject_to: None,
                when_labels: vec!["frontend".to_string()],
                scope: Some("frontend/".to_string()),
            },
        ];

        assert!(validate_dispatch_config(&cfg).is_ok());
    }

    #[test]
    fn validate_all_collects_errors() {
        let mut cfg = valid_config();
        cfg.tracker_kind = "jira".to_string();
        cfg.tracker_api_key = String::new();
        let errors = validate_all(&cfg);
        assert!(
            errors.len() >= 2,
            "expected at least 2 errors, got: {errors:?}"
        );
    }

    #[test]
    fn validate_all_empty_on_valid() {
        let errors = validate_all(&valid_config());
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }
}
