use thiserror::Error;

/// Comprehensive error type for all symphony operations.
#[derive(Debug, Error)]
pub enum SymphonyError {
    #[error("workflow file not found: {path}")]
    MissingWorkflowFile { path: String },

    #[error("failed to parse workflow: {detail}")]
    WorkflowParseError { detail: String },

    #[error("workflow front matter is not a YAML mapping")]
    WorkflowFrontMatterNotAMap,

    #[error("failed to parse template: {detail}")]
    TemplateParseError { detail: String },

    #[error("failed to render template: {detail}")]
    TemplateRenderError { detail: String },

    #[error("unsupported tracker kind: {kind}")]
    UnsupportedTrackerKind { kind: String },

    #[error("tracker API key is required but not configured")]
    MissingTrackerApiKey,

    #[error("tracker project_slug is required")]
    MissingTrackerProjectSlug,

    #[error("tracker API request failed: {detail}")]
    TrackerApiRequest { detail: String },

    #[error("tracker API returned status {status}: {body}")]
    TrackerApiStatus { status: u16, body: String },

    #[error("tracker GraphQL errors: {errors}")]
    TrackerGraphqlErrors { errors: String },

    #[error("tracker returned unknown payload: {detail}")]
    TrackerUnknownPayload { detail: String },

    #[error("tracker response missing end cursor for pagination")]
    TrackerMissingEndCursor,

    #[error("codex command not found: {command}")]
    CodexNotFound { command: String },

    #[error("invalid workspace working directory: {path}")]
    InvalidWorkspaceCwd { path: String },

    #[error("response timed out after {timeout_ms}ms")]
    ResponseTimeout { timeout_ms: u64 },

    #[error("turn timed out after {timeout_ms}ms")]
    TurnTimeout { timeout_ms: u64 },

    #[error("codex process exited with code {code:?}")]
    PortExit { code: Option<i32> },

    #[error("response error: {detail}")]
    ResponseError { detail: String },

    #[error("turn failed: {detail}")]
    TurnFailed { detail: String },

    #[error("turn was cancelled")]
    TurnCancelled,

    #[error("turn requires user input")]
    TurnInputRequired,

    #[error("workspace error: {detail}")]
    WorkspaceError { detail: String },

    #[error("hook '{hook}' failed: {detail}")]
    HookError { hook: String, detail: String },

    #[error("hook '{hook}' timed out after {timeout_ms}ms")]
    HookTimeout { hook: String, timeout_ms: u64 },

    #[error("configuration validation error: {detail}")]
    ConfigValidation { detail: String },
}

pub type Result<T> = std::result::Result<T, SymphonyError>;
