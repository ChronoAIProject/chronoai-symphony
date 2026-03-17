//! Normalize raw GitHub API JSON responses into domain `Issue` values.

use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::Value;

use symphony_core::domain::{BlockerRef, Issue};

/// Normalize a single GitHub issue JSON object into a domain `Issue`.
///
/// Returns `None` if the JSON is missing required fields (e.g., `number`,
/// `node_id`, `title`).
///
/// # State mapping
///
/// GitHub Issues have two native states: `"open"` and `"closed"`. We derive
/// richer workflow states from issue labels:
///
/// - **Open** issue with a label matching an `active_states` entry
///   (case-insensitive) uses that label as its state.
/// - **Open** issue with no matching label defaults to `"Todo"`.
/// - **Closed** issue with a label matching a `terminal_states` entry
///   (case-insensitive) uses that label as its state.
/// - **Closed** issue with no matching label defaults to `"Done"`.
pub fn normalize_github_issue(
    raw: &Value,
    active_states: &[String],
    terminal_states: &[String],
) -> Option<Issue> {
    let number = raw.get("number")?.as_u64()?;
    let node_id = raw.get("node_id")?.as_str()?;
    let title = raw.get("title")?.as_str()?;
    let native_state = raw.get("state")?.as_str()?.to_lowercase();

    let description = raw
        .get("body")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    let html_url = raw
        .get("html_url")
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned());

    // Collect and lowercase all labels.
    let labels: Vec<String> = raw
        .get("labels")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_lowercase())
                .collect()
        })
        .unwrap_or_default();

    // Derive state from labels.
    let state = derive_state(&native_state, &labels, active_states, terminal_states);

    // Extract priority from labels like "priority:1", "priority:2".
    let priority = extract_priority(&labels);

    // Parse blockers from the issue body.
    let blocked_by = description
        .as_deref()
        .map(parse_blockers)
        .unwrap_or_default();

    let created_at = raw
        .get("created_at")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());

    let updated_at = raw
        .get("updated_at")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<DateTime<Utc>>().ok());

    Some(Issue {
        id: node_id.to_owned(),
        identifier: format!("#{number}"),
        title: title.to_owned(),
        description,
        priority,
        state,
        branch_name: None,
        url: html_url,
        labels,
        blocked_by,
        created_at,
        updated_at,
    })
}

/// Derive the workflow state from the GitHub native state and labels.
fn derive_state(
    native_state: &str,
    labels: &[String],
    active_states: &[String],
    terminal_states: &[String],
) -> String {
    match native_state {
        "open" => {
            for label in labels {
                for active in active_states {
                    if label == &active.to_lowercase() {
                        return active.clone();
                    }
                }
            }
            "Todo".to_owned()
        }
        "closed" => {
            for label in labels {
                for terminal in terminal_states {
                    if label == &terminal.to_lowercase() {
                        return terminal.clone();
                    }
                }
            }
            "Done".to_owned()
        }
        _ => "Todo".to_owned(),
    }
}

/// Extract a numeric priority from labels matching `priority:<N>`.
fn extract_priority(labels: &[String]) -> Option<i32> {
    for label in labels {
        if let Some(suffix) = label.strip_prefix("priority:") {
            if let Ok(p) = suffix.trim().parse::<i32>() {
                return Some(p);
            }
        }
    }
    None
}

/// Parse blocker references from issue body text.
///
/// Matches patterns like "blocked by #N" and "depends on #N"
/// (case-insensitive).
fn parse_blockers(body: &str) -> Vec<BlockerRef> {
    let re = Regex::new(r"(?i)(?:blocked\s+by|depends\s+on)\s+#(\d+)")
        .expect("invalid blocker regex");

    re.captures_iter(body)
        .map(|cap| {
            let num = &cap[1];
            BlockerRef {
                id: None,
                identifier: Some(format!("#{num}")),
                state: None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn active_states() -> Vec<String> {
        vec![
            "Todo".to_owned(),
            "In Progress".to_owned(),
        ]
    }

    fn terminal_states() -> Vec<String> {
        vec!["Done".to_owned(), "Cancelled".to_owned()]
    }

    fn sample_issue() -> Value {
        json!({
            "number": 42,
            "node_id": "MDU6SXNzdWU0Mg==",
            "title": "Fix the widget",
            "state": "open",
            "body": "This widget is broken.\n\nBlocked by #10\nAlso depends on #20",
            "html_url": "https://github.com/owner/repo/issues/42",
            "labels": [
                { "name": "In Progress" },
                { "name": "priority:2" },
                { "name": "bug" }
            ],
            "created_at": "2025-01-15T10:00:00Z",
            "updated_at": "2025-01-16T12:30:00Z"
        })
    }

    #[test]
    fn normalizes_open_issue_with_active_label() {
        let issue = normalize_github_issue(
            &sample_issue(),
            &active_states(),
            &terminal_states(),
        )
        .unwrap();

        assert_eq!(issue.id, "MDU6SXNzdWU0Mg==");
        assert_eq!(issue.identifier, "#42");
        assert_eq!(issue.title, "Fix the widget");
        assert_eq!(issue.state, "In Progress");
        assert_eq!(issue.priority, Some(2));
        assert_eq!(issue.url.as_deref(), Some("https://github.com/owner/repo/issues/42"));
        assert!(issue.branch_name.is_none());
        assert_eq!(issue.labels, vec!["in progress", "priority:2", "bug"]);
    }

    #[test]
    fn extracts_blockers_from_body() {
        let issue = normalize_github_issue(
            &sample_issue(),
            &active_states(),
            &terminal_states(),
        )
        .unwrap();

        assert_eq!(issue.blocked_by.len(), 2);
        assert_eq!(
            issue.blocked_by[0].identifier.as_deref(),
            Some("#10")
        );
        assert_eq!(
            issue.blocked_by[1].identifier.as_deref(),
            Some("#20")
        );
    }

    #[test]
    fn open_issue_without_active_label_defaults_to_todo() {
        let raw = json!({
            "number": 1,
            "node_id": "node-1",
            "title": "New feature",
            "state": "open",
            "body": null,
            "html_url": "https://github.com/o/r/issues/1",
            "labels": [],
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z"
        });

        let issue = normalize_github_issue(
            &raw,
            &active_states(),
            &terminal_states(),
        )
        .unwrap();

        assert_eq!(issue.state, "Todo");
    }

    #[test]
    fn closed_issue_defaults_to_done() {
        let raw = json!({
            "number": 2,
            "node_id": "node-2",
            "title": "Old bug",
            "state": "closed",
            "body": "",
            "html_url": "https://github.com/o/r/issues/2",
            "labels": [],
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-02T00:00:00Z"
        });

        let issue = normalize_github_issue(
            &raw,
            &active_states(),
            &terminal_states(),
        )
        .unwrap();

        assert_eq!(issue.state, "Done");
    }

    #[test]
    fn closed_issue_with_terminal_label() {
        let raw = json!({
            "number": 3,
            "node_id": "node-3",
            "title": "Wont fix",
            "state": "closed",
            "body": null,
            "html_url": "https://github.com/o/r/issues/3",
            "labels": [{ "name": "Cancelled" }],
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-02T00:00:00Z"
        });

        let issue = normalize_github_issue(
            &raw,
            &active_states(),
            &terminal_states(),
        )
        .unwrap();

        assert_eq!(issue.state, "Cancelled");
    }

    #[test]
    fn parses_dates() {
        let issue = normalize_github_issue(
            &sample_issue(),
            &active_states(),
            &terminal_states(),
        )
        .unwrap();

        assert!(issue.created_at.is_some());
        assert!(issue.updated_at.is_some());
    }

    #[test]
    fn missing_required_fields_returns_none() {
        let raw = json!({ "title": "No number" });
        assert!(normalize_github_issue(
            &raw,
            &active_states(),
            &terminal_states()
        )
        .is_none());
    }

    #[test]
    fn no_priority_label_returns_none_priority() {
        let raw = json!({
            "number": 5,
            "node_id": "node-5",
            "title": "No priority",
            "state": "open",
            "body": null,
            "html_url": "https://github.com/o/r/issues/5",
            "labels": [{ "name": "enhancement" }],
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T00:00:00Z"
        });

        let issue = normalize_github_issue(
            &raw,
            &active_states(),
            &terminal_states(),
        )
        .unwrap();

        assert!(issue.priority.is_none());
    }

    #[test]
    fn parse_blockers_case_insensitive() {
        let body = "BLOCKED BY #5\nDEPENDS ON #7";
        let blockers = parse_blockers(body);
        assert_eq!(blockers.len(), 2);
        assert_eq!(blockers[0].identifier.as_deref(), Some("#5"));
        assert_eq!(blockers[1].identifier.as_deref(), Some("#7"));
    }

    #[test]
    fn parse_blockers_empty_body() {
        let blockers = parse_blockers("");
        assert!(blockers.is_empty());
    }
}
