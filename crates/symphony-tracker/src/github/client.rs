//! GitHub Issues client implementing the `IssueTracker` trait.

use async_trait::async_trait;
use reqwest::header::{self, HeaderMap, HeaderValue};
use serde_json::Value;
use tracing::{info, warn};

use symphony_core::domain::Issue;
use symphony_core::error::SymphonyError;

use super::normalization::normalize_github_issue;
use crate::traits::IssueTracker;

/// HTTP client for the GitHub Issues REST API.
pub struct GitHubClient {
    http: reqwest::Client,
    endpoint: String,
    owner: String,
    repo: String,
    active_states: Vec<String>,
    terminal_states: Vec<String>,
}

impl GitHubClient {
    /// Create a new GitHub client.
    ///
    /// # Arguments
    ///
    /// * `token` - GitHub personal access token for authentication.
    /// * `project_slug` - Repository in `"owner/repo"` format.
    /// * `active_states` - Label names representing active workflow states.
    /// * `terminal_states` - Label names representing terminal workflow states.
    /// * `endpoint` - Optional API base URL (defaults to `https://api.github.com`).
    pub fn new(
        token: &str,
        project_slug: &str,
        active_states: Vec<String>,
        terminal_states: Vec<String>,
        endpoint: Option<&str>,
    ) -> Result<Self, SymphonyError> {
        let (owner, repo) = parse_project_slug(project_slug)?;

        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(
                |e| SymphonyError::TrackerApiRequest {
                    detail: format!("invalid auth token header value: {e}"),
                },
            )?,
        );
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        headers.insert(
            header::USER_AGENT,
            HeaderValue::from_static("chronoai-symphony/0.1"),
        );

        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|e| SymphonyError::TrackerApiRequest {
                detail: format!("failed to build HTTP client: {e}"),
            })?;

        let base = endpoint.unwrap_or("https://api.github.com");

        Ok(Self {
            http,
            endpoint: base.to_owned(),
            owner,
            repo,
            active_states,
            terminal_states,
        })
    }

    /// Fetch all pages of issues for the given GitHub state parameter.
    async fn fetch_paginated(
        &self,
        state_param: &str,
    ) -> Result<Vec<Value>, SymphonyError> {
        let mut all_items: Vec<Value> = Vec::new();
        let mut page: u32 = 1;

        loop {
            let url = format!(
                "{}/repos/{}/{}/issues?state={}&per_page=100&page={}",
                self.endpoint, self.owner, self.repo, state_param, page
            );

            info!(url = %url, page, "fetching GitHub issues");

            let response = self
                .http
                .get(&url)
                .send()
                .await
                .map_err(|e| SymphonyError::TrackerApiRequest {
                    detail: format!("HTTP request failed: {e}"),
                })?;

            check_rate_limit(&response);

            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(SymphonyError::TrackerApiStatus {
                    status: status.as_u16(),
                    body,
                });
            }

            let items: Vec<Value> = response.json().await.map_err(|e| {
                SymphonyError::TrackerUnknownPayload {
                    detail: format!("failed to parse JSON array: {e}"),
                }
            })?;

            let count = items.len();
            all_items.extend(items);

            // GitHub returns fewer than per_page items on the last page.
            if count < 100 {
                break;
            }
            page += 1;
        }

        Ok(all_items)
    }

    /// Fetch a single issue by its number.
    async fn fetch_issue_by_number(
        &self,
        number: u64,
    ) -> Result<Value, SymphonyError> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}",
            self.endpoint, self.owner, self.repo, number
        );

        let response = self
            .http
            .get(&url)
            .send()
            .await
            .map_err(|e| SymphonyError::TrackerApiRequest {
                detail: format!("HTTP request failed: {e}"),
            })?;

        check_rate_limit(&response);

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SymphonyError::TrackerApiStatus {
                status: status.as_u16(),
                body,
            });
        }

        response.json().await.map_err(|e| {
            SymphonyError::TrackerUnknownPayload {
                detail: format!("failed to parse issue JSON: {e}"),
            }
        })
    }

    /// Normalize raw JSON issues, filtering out pull requests.
    ///
    /// GitHub's `/issues` endpoint includes pull requests. We exclude them
    /// by checking for the `pull_request` key.
    fn normalize_items(&self, items: &[Value]) -> Vec<Issue> {
        items
            .iter()
            .filter(|item| {
                !item
                    .get("pull_request")
                    .is_some_and(|v| !v.is_null())
            })
            .filter_map(|item| {
                normalize_github_issue(
                    item,
                    &self.active_states,
                    &self.terminal_states,
                )
            })
            .collect()
    }
}

#[async_trait]
impl IssueTracker for GitHubClient {
    /// Fetch open issues that are candidates for agent processing.
    async fn fetch_candidate_issues(
        &self,
    ) -> Result<Vec<Issue>, SymphonyError> {
        let items = self.fetch_paginated("open").await?;
        let issues = self.normalize_items(&items);

        info!(count = issues.len(), "fetched candidate issues");
        Ok(issues)
    }

    /// Fetch issues filtered by the given state names.
    ///
    /// Queries open and/or closed endpoints based on whether the requested
    /// states overlap with active or terminal state lists.
    async fn fetch_issues_by_states(
        &self,
        states: &[String],
    ) -> Result<Vec<Issue>, SymphonyError> {
        if states.is_empty() {
            return Ok(Vec::new());
        }

        let states_lower: Vec<String> =
            states.iter().map(|s| s.to_lowercase()).collect();

        let has_terminal = states_lower.iter().any(|s| {
            self.terminal_states
                .iter()
                .any(|ts| ts.to_lowercase() == *s)
        });
        let has_active = states_lower.iter().any(|s| {
            self.active_states
                .iter()
                .any(|a| a.to_lowercase() == *s)
                || *s == "todo"
        });

        let mut all_issues: Vec<Issue> = Vec::new();

        if has_active {
            let items = self.fetch_paginated("open").await?;
            all_issues.extend(self.normalize_items(&items));
        }

        if has_terminal {
            let items = self.fetch_paginated("closed").await?;
            all_issues.extend(self.normalize_items(&items));
        }

        // Keep only issues whose state matches a requested state.
        let filtered: Vec<Issue> = all_issues
            .into_iter()
            .filter(|issue| {
                states_lower.contains(&issue.state.to_lowercase())
            })
            .collect();

        info!(
            count = filtered.len(),
            ?states,
            "fetched issues by states"
        );
        Ok(filtered)
    }

    /// Fetch current state for issues identified by their identifiers.
    ///
    /// Identifiers are in `"#N"` format. Each issue is fetched individually
    /// by its number.
    async fn fetch_issue_states_by_ids(
        &self,
        ids: &[String],
    ) -> Result<Vec<Issue>, SymphonyError> {
        let mut results: Vec<Issue> = Vec::new();

        for id in ids {
            match extract_issue_number(id) {
                Some(n) => match self.fetch_issue_by_number(n).await {
                    Ok(raw) => {
                        if let Some(issue) = normalize_github_issue(
                            &raw,
                            &self.active_states,
                            &self.terminal_states,
                        ) {
                            results.push(issue);
                        }
                    }
                    Err(e) => {
                        warn!(
                            identifier = %id,
                            error = %e,
                            "failed to fetch issue state"
                        );
                    }
                },
                None => {
                    warn!(
                        identifier = %id,
                        "cannot extract issue number from identifier"
                    );
                }
            }
        }

        info!(
            count = results.len(),
            requested = ids.len(),
            "fetched issue states by IDs"
        );
        Ok(results)
    }
}

/// Parse an `"owner/repo"` slug into its two components.
fn parse_project_slug(
    slug: &str,
) -> Result<(String, String), SymphonyError> {
    let parts: Vec<&str> = slug.splitn(2, '/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        return Err(SymphonyError::MissingTrackerProjectSlug);
    }
    Ok((parts[0].to_owned(), parts[1].to_owned()))
}

/// Extract the issue number from an identifier like `"#42"` or `"42"`.
fn extract_issue_number(id: &str) -> Option<u64> {
    let trimmed = id.strip_prefix('#').unwrap_or(id);
    trimmed.parse::<u64>().ok()
}

/// Log a warning if the GitHub rate limit is running low.
fn check_rate_limit(response: &reqwest::Response) {
    if let Some(remaining) = response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u32>().ok())
    {
        if remaining < 50 {
            warn!(remaining, "GitHub API rate limit running low");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_project_slug_valid() {
        let (owner, repo) =
            parse_project_slug("octocat/hello-world").unwrap();
        assert_eq!(owner, "octocat");
        assert_eq!(repo, "hello-world");
    }

    #[test]
    fn parse_project_slug_invalid_no_slash() {
        assert!(parse_project_slug("no-slash").is_err());
    }

    #[test]
    fn parse_project_slug_invalid_empty_parts() {
        assert!(parse_project_slug("/repo").is_err());
        assert!(parse_project_slug("owner/").is_err());
    }

    #[test]
    fn extract_issue_number_with_hash() {
        assert_eq!(extract_issue_number("#42"), Some(42));
    }

    #[test]
    fn extract_issue_number_without_hash() {
        assert_eq!(extract_issue_number("99"), Some(99));
    }

    #[test]
    fn extract_issue_number_invalid() {
        assert_eq!(extract_issue_number("abc"), None);
    }
}
