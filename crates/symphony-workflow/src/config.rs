//! Convenience layer for building `ServiceConfig` from workflow YAML.
//!
//! Provides helper functions for environment variable resolution, home
//! directory expansion, and YAML value coercion.

use std::env;

use symphony_core::domain::{ServiceConfig, WorkflowDefinition};
use symphony_core::error::SymphonyError;

/// Build a `ServiceConfig` from a parsed `WorkflowDefinition`.
///
/// Delegates to `ServiceConfig::from_workflow()` in symphony-core, which
/// resolves environment variables, applies defaults, and validates required
/// fields.
pub fn build_config(workflow: &WorkflowDefinition) -> Result<ServiceConfig, SymphonyError> {
    ServiceConfig::from_workflow(workflow)
}

/// Resolve an environment variable reference.
///
/// If `value` starts with `$`, the remainder is treated as an environment
/// variable name and looked up. Returns `None` if the variable is unset
/// or empty. If `value` does not start with `$`, returns `Some(value)`.
pub fn resolve_env_var(value: &str) -> Option<String> {
    if let Some(var_name) = value.strip_prefix('$') {
        match env::var(var_name) {
            Ok(v) if !v.is_empty() => Some(v),
            _ => None,
        }
    } else {
        Some(value.to_string())
    }
}

/// Expand a leading `~` in a path to the user's home directory.
///
/// If the path does not start with `~`, it is returned unchanged.
/// If the home directory cannot be determined, `~` is left as-is.
pub fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix('~') {
        match env::var("HOME").or_else(|_| env::var("USERPROFILE")) {
            Ok(home) => {
                if rest.is_empty() {
                    home
                } else {
                    format!("{home}{rest}")
                }
            }
            Err(_) => path.to_string(),
        }
    } else {
        path.to_string()
    }
}

/// Coerce a YAML value to `u64`.
///
/// Handles both integer values and string representations of integers.
pub fn coerce_to_u64(value: &serde_yaml_ng::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<u64>().ok()))
}

/// Coerce a YAML value to `i64`.
///
/// Handles both integer values and string representations of integers.
pub fn coerce_to_i64(value: &serde_yaml_ng::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse::<i64>().ok()))
}

/// Coerce a YAML value to `u32`.
///
/// Handles both integer values and string representations of integers.
/// Returns `None` if the value is out of `u32` range.
pub fn coerce_to_u32(value: &serde_yaml_ng::Value) -> Option<u32> {
    coerce_to_u64(value).and_then(|v| u32::try_from(v).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- resolve_env_var --

    #[test]
    fn resolve_env_var_literal_value() {
        assert_eq!(resolve_env_var("hello"), Some("hello".to_string()));
    }

    #[test]
    fn resolve_env_var_existing_var() {
        unsafe { env::set_var("SYMPHONY_TEST_VAR_1", "secret123") };
        assert_eq!(
            resolve_env_var("$SYMPHONY_TEST_VAR_1"),
            Some("secret123".to_string())
        );
        unsafe { env::remove_var("SYMPHONY_TEST_VAR_1") };
    }

    #[test]
    fn resolve_env_var_missing_var() {
        unsafe { env::remove_var("SYMPHONY_TEST_NONEXISTENT") };
        assert_eq!(resolve_env_var("$SYMPHONY_TEST_NONEXISTENT"), None);
    }

    #[test]
    fn resolve_env_var_empty_var() {
        unsafe { env::set_var("SYMPHONY_TEST_EMPTY", "") };
        assert_eq!(resolve_env_var("$SYMPHONY_TEST_EMPTY"), None);
        unsafe { env::remove_var("SYMPHONY_TEST_EMPTY") };
    }

    // -- expand_home --

    #[test]
    fn expand_home_with_tilde() {
        let home = env::var("HOME")
            .or_else(|_| env::var("USERPROFILE"))
            .unwrap_or_default();
        assert_eq!(expand_home("~/projects"), format!("{home}/projects"));
    }

    #[test]
    fn expand_home_tilde_only() {
        let home = env::var("HOME")
            .or_else(|_| env::var("USERPROFILE"))
            .unwrap_or_default();
        assert_eq!(expand_home("~"), home);
    }

    #[test]
    fn expand_home_no_tilde() {
        assert_eq!(expand_home("/usr/local"), "/usr/local");
    }

    #[test]
    fn expand_home_relative_path() {
        assert_eq!(expand_home("relative/path"), "relative/path");
    }

    // -- coerce_to_u64 --

    #[test]
    fn coerce_u64_from_integer() {
        let val = serde_yaml_ng::Value::Number(serde_yaml_ng::Number::from(42u64));
        assert_eq!(coerce_to_u64(&val), Some(42));
    }

    #[test]
    fn coerce_u64_from_string() {
        let val = serde_yaml_ng::Value::String("100".to_string());
        assert_eq!(coerce_to_u64(&val), Some(100));
    }

    #[test]
    fn coerce_u64_from_invalid_string() {
        let val = serde_yaml_ng::Value::String("not_a_number".to_string());
        assert_eq!(coerce_to_u64(&val), None);
    }

    #[test]
    fn coerce_u64_from_null() {
        assert_eq!(coerce_to_u64(&serde_yaml_ng::Value::Null), None);
    }

    // -- coerce_to_i64 --

    #[test]
    fn coerce_i64_from_negative() {
        let val: serde_yaml_ng::Value = serde_yaml_ng::from_str("-5").unwrap();
        assert_eq!(coerce_to_i64(&val), Some(-5));
    }

    #[test]
    fn coerce_i64_from_string() {
        let val = serde_yaml_ng::Value::String("-42".to_string());
        assert_eq!(coerce_to_i64(&val), Some(-42));
    }

    // -- coerce_to_u32 --

    #[test]
    fn coerce_u32_from_integer() {
        let val = serde_yaml_ng::Value::Number(serde_yaml_ng::Number::from(10u64));
        assert_eq!(coerce_to_u32(&val), Some(10));
    }

    #[test]
    fn coerce_u32_overflow() {
        let val = serde_yaml_ng::Value::Number(serde_yaml_ng::Number::from(u64::MAX));
        assert_eq!(coerce_to_u32(&val), None);
    }

    #[test]
    fn coerce_u32_from_string() {
        let val = serde_yaml_ng::Value::String("255".to_string());
        assert_eq!(coerce_to_u32(&val), Some(255));
    }

    // -- build_config --

    #[test]
    fn build_config_from_workflow() {
        unsafe { env::set_var("SYMPHONY_CFG_TEST_KEY", "test_token") };
        let wf = WorkflowDefinition {
            config: serde_yaml_ng::from_str(
                r#"
tracker:
  kind: github
  api_key: $SYMPHONY_CFG_TEST_KEY
  project_slug: owner/repo
codex:
  command: codex
"#,
            )
            .unwrap(),
            prompt_template: "hello".to_string(),
        };
        let cfg = build_config(&wf).unwrap();
        assert_eq!(cfg.tracker_kind, "github");
        assert_eq!(cfg.codex_command, "codex");
        assert_eq!(cfg.tracker_api_key, "test_token");
        unsafe { env::remove_var("SYMPHONY_CFG_TEST_KEY") };
    }
}
