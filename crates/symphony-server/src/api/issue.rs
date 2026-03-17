//! GET /api/v1/{issue_identifier} endpoint.
//!
//! Returns details for a single issue from the current orchestrator snapshot.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};

use crate::router::AppState;

/// Look up a specific issue by its identifier (e.g., `#42` or `_42`).
///
/// Searches both the `running` and `retrying` arrays in the orchestrator
/// snapshot. Returns the matching entry or a 404 error.
///
/// # Path parameters
///
/// - `issue_identifier` - The issue identifier to look up. Matches against
///   the `identifier` field in running entries and the `identifier` field
///   in retry entries.
///
/// # Errors
///
/// Returns `404 Not Found` with a JSON error body if the issue is not in
/// the current snapshot.
pub async fn get_issue(
    State(state): State<Arc<AppState>>,
    Path(issue_identifier): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let snapshot = (state.snapshot_fn)();

    // Search running entries.
    if let Some(entries) = snapshot.get("running").and_then(|v| v.as_array()) {
        for entry in entries {
            if matches_identifier(entry, &issue_identifier) {
                return Ok(Json(json!({
                    "found_in": "running",
                    "issue": entry
                })));
            }
        }
    }

    // Search retry entries.
    if let Some(entries) = snapshot.get("retrying").and_then(|v| v.as_array()) {
        for entry in entries {
            if matches_identifier(entry, &issue_identifier) {
                return Ok(Json(json!({
                    "found_in": "retrying",
                    "issue": entry
                })));
            }
        }
    }

    Err((
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": "not_found",
            "message": format!(
                "Issue '{}' is not currently running or in the retry queue",
                issue_identifier
            )
        })),
    ))
}

/// Check whether a JSON entry's `identifier` field matches the given identifier.
///
/// Performs a case-insensitive, whitespace-trimmed comparison.
fn matches_identifier(entry: &Value, identifier: &str) -> bool {
    entry
        .get("identifier")
        .and_then(|v| v.as_str())
        .is_some_and(|entry_id| {
            entry_id.trim().eq_ignore_ascii_case(identifier.trim())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn matches_identifier_exact() {
        let entry = json!({"identifier": "#42"});
        assert!(matches_identifier(&entry, "#42"));
    }

    #[test]
    fn matches_identifier_case_insensitive() {
        let entry = json!({"identifier": "#ABC"});
        assert!(matches_identifier(&entry, "#abc"));
    }

    #[test]
    fn matches_identifier_with_whitespace() {
        let entry = json!({"identifier": " #42 "});
        assert!(matches_identifier(&entry, "#42"));
    }

    #[test]
    fn does_not_match_different_identifier() {
        let entry = json!({"identifier": "#42"});
        assert!(!matches_identifier(&entry, "#99"));
    }

    #[test]
    fn does_not_match_missing_field() {
        let entry = json!({"title": "No identifier"});
        assert!(!matches_identifier(&entry, "#42"));
    }
}
