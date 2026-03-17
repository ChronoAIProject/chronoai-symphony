use thiserror::Error;

/// GitHub-specific errors that are mapped to `SymphonyError` at the boundary.
#[derive(Debug, Error)]
pub enum GitHubError {
    #[error("HTTP transport error: {0}")]
    Transport(#[from] reqwest::Error),

    #[error("GitHub API returned status {status}: {body}")]
    ApiStatus { status: u16, body: String },

    #[error("rate limit exhausted, resets at {reset_at}")]
    RateLimited { reset_at: String },

    #[error("malformed response: {detail}")]
    MalformedResponse { detail: String },

    #[error("invalid project slug '{slug}': expected 'owner/repo' format")]
    InvalidProjectSlug { slug: String },
}
