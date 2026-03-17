//! WORKFLOW.md parsing per Section 5.2 of the Symphony spec.
//!
//! Reads a WORKFLOW.md file, extracts optional YAML front matter delimited
//! by `---` lines, and treats the remainder as the prompt template body.

use std::fs;
use std::path::Path;

use symphony_core::domain::WorkflowDefinition;
use symphony_core::error::SymphonyError;

/// Load and parse a WORKFLOW.md file into a `WorkflowDefinition`.
///
/// # Parsing rules
///
/// - If the file starts with `---`, lines up to the next `---` are parsed
///   as YAML front matter.
/// - The remaining lines become the prompt body (trimmed).
/// - If no front matter is present, the entire file is the prompt body
///   and `config` is set to an empty YAML mapping.
///
/// # Errors
///
/// - `MissingWorkflowFile` if the file cannot be read.
/// - `WorkflowParseError` if YAML is syntactically invalid.
/// - `WorkflowFrontMatterNotAMap` if YAML parses to a non-mapping type.
pub fn load_workflow(path: &Path) -> Result<WorkflowDefinition, SymphonyError> {
    let content = fs::read_to_string(path).map_err(|_| SymphonyError::MissingWorkflowFile {
        path: path.display().to_string(),
    })?;

    parse_workflow_str(&content)
}

/// Parse workflow content from a string (useful for testing and reload).
pub fn parse_workflow_str(content: &str) -> Result<WorkflowDefinition, SymphonyError> {
    let (yaml_str, body) = split_front_matter(content);

    let config = match yaml_str {
        Some(yaml) => {
            let value: serde_yaml_ng::Value =
                serde_yaml_ng::from_str(&yaml).map_err(|e| SymphonyError::WorkflowParseError {
                    detail: e.to_string(),
                })?;

            // Null (empty front matter) is fine -- treat as empty map.
            if value.is_null() {
                serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new())
            } else if value.is_mapping() {
                value
            } else {
                return Err(SymphonyError::WorkflowFrontMatterNotAMap);
            }
        }
        None => serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new()),
    };

    let prompt_template = body.trim().to_string();

    Ok(WorkflowDefinition {
        config,
        prompt_template,
    })
}

/// Split content into optional YAML front matter and the remaining body.
///
/// Returns `(Some(yaml_text), body)` when delimiters are found,
/// or `(None, full_content)` when no front matter is present.
fn split_front_matter(content: &str) -> (Option<String>, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (None, content.to_string());
    }

    // Find the opening delimiter line.
    let after_first = match trimmed.strip_prefix("---") {
        Some(rest) => rest,
        None => return (None, content.to_string()),
    };

    // Skip the rest of the opening delimiter line (e.g., "---\n").
    let after_first_line = match after_first.find('\n') {
        Some(pos) => &after_first[pos + 1..],
        None => {
            // File is just "---" with nothing after.
            return (Some(String::new()), String::new());
        }
    };

    // Find the closing `---` on its own line.
    if let Some(end_pos) = find_closing_delimiter(after_first_line) {
        let yaml_text = after_first_line[..end_pos].to_string();
        let body_start = end_pos + 3; // skip "---"
        let body = if body_start < after_first_line.len() {
            let rest = &after_first_line[body_start..];
            // Skip the newline after the closing delimiter.
            rest.strip_prefix('\n').unwrap_or(rest).to_string()
        } else {
            String::new()
        };
        (Some(yaml_text), body)
    } else {
        // No closing delimiter found -- entire file after first --- is YAML.
        (Some(after_first_line.to_string()), String::new())
    }
}

/// Find the byte offset of a `---` closing delimiter that starts at the
/// beginning of a line.
fn find_closing_delimiter(s: &str) -> Option<usize> {
    // Check if the string itself starts with ---
    if s.starts_with("---") && (s.len() == 3 || s.as_bytes().get(3) == Some(&b'\n')) {
        return Some(0);
    }

    // Search for \n---\n or \n--- at end of string.
    let mut search_start = 0;
    while let Some(pos) = s[search_start..].find("\n---") {
        let absolute_pos = search_start + pos + 1; // position of first '-'
        let after_dashes = absolute_pos + 3;
        if after_dashes >= s.len() || s.as_bytes()[after_dashes] == b'\n' {
            return Some(absolute_pos);
        }
        search_start = absolute_pos + 3;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_with_front_matter_and_body() {
        let content = "\
---
tracker_kind: github
model: o3
---
You are a helpful assistant.
Fix the issue described below.
";
        let wf = parse_workflow_str(content).unwrap();
        assert!(wf.config.is_mapping());
        let map = wf.config.as_mapping().unwrap();
        assert_eq!(
            map.get(serde_yaml_ng::Value::String("tracker_kind".into()))
                .and_then(|v| v.as_str()),
            Some("github")
        );
        assert_eq!(
            map.get(serde_yaml_ng::Value::String("model".into()))
                .and_then(|v| v.as_str()),
            Some("o3")
        );
        assert!(wf.prompt_template.contains("helpful assistant"));
        assert!(wf.prompt_template.contains("Fix the issue"));
    }

    #[test]
    fn parse_no_front_matter() {
        let content = "Just a plain prompt with no YAML.";
        let wf = parse_workflow_str(content).unwrap();
        assert!(wf.config.as_mapping().unwrap().is_empty());
        assert_eq!(wf.prompt_template, "Just a plain prompt with no YAML.");
    }

    #[test]
    fn parse_empty_file() {
        let wf = parse_workflow_str("").unwrap();
        assert!(wf.config.as_mapping().unwrap().is_empty());
        assert_eq!(wf.prompt_template, "");
    }

    #[test]
    fn parse_invalid_yaml() {
        let content = "\
---
[invalid: yaml: :
---
body
";
        let err = parse_workflow_str(content).unwrap_err();
        match err {
            SymphonyError::WorkflowParseError { .. } => {}
            other => panic!("expected WorkflowParseError, got: {other:?}"),
        }
    }

    #[test]
    fn parse_non_map_yaml() {
        let content = "\
---
- item1
- item2
---
body
";
        let err = parse_workflow_str(content).unwrap_err();
        match err {
            SymphonyError::WorkflowFrontMatterNotAMap => {}
            other => panic!("expected WorkflowFrontMatterNotAMap, got: {other:?}"),
        }
    }

    #[test]
    fn parse_empty_front_matter() {
        let content = "\
---
---
Just the body.
";
        let wf = parse_workflow_str(content).unwrap();
        assert!(wf.config.as_mapping().unwrap().is_empty());
        assert_eq!(wf.prompt_template, "Just the body.");
    }

    #[test]
    fn load_missing_file() {
        let err = load_workflow(Path::new("/nonexistent/WORKFLOW.md")).unwrap_err();
        match err {
            SymphonyError::MissingWorkflowFile { path } => {
                assert!(path.contains("WORKFLOW.md"));
            }
            other => panic!("expected MissingWorkflowFile, got: {other:?}"),
        }
    }

    #[test]
    fn parse_front_matter_with_nested_yaml() {
        let content = "\
---
tracker:
  kind: github
  api_key: \"$GITHUB_TOKEN\"
codex:
  command: codex
  model: o3
---
Fix issue {{ issue.identifier }}: {{ issue.title }}
";
        let wf = parse_workflow_str(content).unwrap();
        let map = wf.config.as_mapping().unwrap();
        let tracker = map
            .get(serde_yaml_ng::Value::String("tracker".into()))
            .unwrap()
            .as_mapping()
            .unwrap();
        assert_eq!(
            tracker
                .get(serde_yaml_ng::Value::String("kind".into()))
                .and_then(|v| v.as_str()),
            Some("github")
        );
        assert!(wf.prompt_template.starts_with("Fix issue"));
    }
}
