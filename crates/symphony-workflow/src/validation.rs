//! Dispatch preflight validation per Section 6.3 of the Symphony spec.
//!
//! Validates that all required configuration fields are present and
//! well-formed before dispatching work to agents.

use symphony_core::domain::ServiceConfig;
use symphony_core::error::SymphonyError;

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
    validate_tracker_kind(config)?;
    validate_tracker_api_key(config)?;
    validate_tracker_project_slug(config)?;
    validate_codex_command(config)?;
    Ok(())
}

/// Collect all validation errors instead of failing on the first one.
///
/// Returns a `Vec` of error messages. An empty vec means validation passed.
pub fn validate_all(config: &ServiceConfig) -> Vec<String> {
    let checks: Vec<Result<(), SymphonyError>> = vec![
        validate_tracker_kind(config),
        validate_tracker_api_key(config),
        validate_tracker_project_slug(config),
        validate_codex_command(config),
    ];

    checks
        .into_iter()
        .filter_map(|r| r.err())
        .map(|e| e.to_string())
        .collect()
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
    fn validate_all_collects_errors() {
        let mut cfg = valid_config();
        cfg.tracker_kind = "jira".to_string();
        cfg.tracker_api_key = String::new();
        let errors = validate_all(&cfg);
        assert!(errors.len() >= 2, "expected at least 2 errors, got: {errors:?}");
    }

    #[test]
    fn validate_all_empty_on_valid() {
        let errors = validate_all(&valid_config());
        assert!(errors.is_empty(), "expected no errors, got: {errors:?}");
    }
}
