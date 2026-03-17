//! Symphony CLI entry point.
//!
//! Parses command-line arguments, initializes logging, loads the workflow
//! configuration, and starts the orchestrator. Optionally starts an HTTP
//! server for the dashboard and REST API.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use clap::Parser;

/// Orchestrate coding agents to work on GitHub issues.
#[derive(Parser)]
#[command(
    name = "symphony",
    about = "Orchestrate coding agents to work on GitHub issues"
)]
struct Args {
    /// Path to WORKFLOW.md file.
    #[arg(default_value = "./WORKFLOW.md")]
    workflow_path: PathBuf,

    /// Enable HTTP server on specified port.
    #[arg(long)]
    port: Option<u16>,

    /// Use pretty (non-JSON) log output for local development.
    #[arg(long)]
    pretty_logs: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // 1. Configure logging.
    if args.pretty_logs {
        symphony_logging::setup::init_logging_pretty();
    } else {
        symphony_logging::setup::init_logging();
    }

    tracing::info!("starting Symphony");

    // 2. Load and validate workflow.
    let workflow = symphony_workflow::loader::load_workflow(&args.workflow_path)
        .map_err(|e| {
            tracing::error!(error = %e, "failed to load workflow");
            anyhow::anyhow!("failed to load workflow: {e}")
        })?;

    let config = symphony_core::domain::ServiceConfig::from_workflow(&workflow)
        .map_err(|e| {
            tracing::error!(error = %e, "failed to build config from workflow");
            anyhow::anyhow!("failed to build config: {e}")
        })?;

    // 3. Validate dispatch config.
    symphony_workflow::validation::validate_dispatch_config(&config)
        .map_err(|e| {
            tracing::error!(error = %e, "dispatch config validation failed");
            anyhow::anyhow!("dispatch config validation failed: {e}")
        })?;

    // 4. Initialize components.
    let endpoint = if config.tracker_endpoint.is_empty() {
        None
    } else {
        Some(config.tracker_endpoint.as_str())
    };

    let github_client = symphony_tracker::github::client::GitHubClient::new(
        &config.tracker_api_key,
        &config.tracker_project_slug,
        config.tracker_active_states.clone(),
        config.tracker_terminal_states.clone(),
        endpoint,
    )
    .map_err(|e| {
        tracing::error!(error = %e, "failed to create GitHub client");
        anyhow::anyhow!("failed to create GitHub client: {e}")
    })?;

    let tracker: Arc<dyn symphony_tracker::traits::IssueTracker> =
        Arc::new(github_client);

    let workspace_manager = Arc::new(
        symphony_workspace::manager::WorkspaceManager::new(
            config.workspace_root.clone(),
            config.hooks.after_create.clone(),
            config.hooks.before_run.clone(),
            config.hooks.after_run.clone(),
            config.hooks.before_remove.clone(),
            config.hooks.timeout_ms,
        ),
    );

    let agent_runner = Arc::new(
        symphony_agent::runner::AgentRunner::new(config.clone()),
    );

    // 5. Startup terminal cleanup.
    symphony_orchestrator::cleanup::startup_terminal_cleanup(
        tracker.as_ref(),
        workspace_manager.as_ref(),
        &config.tracker_terminal_states,
    )
    .await;

    // 6. Create orchestrator.
    let mut orchestrator = symphony_orchestrator::orchestrator::Orchestrator::new(
        config.clone(),
        workflow.prompt_template.clone(),
        tracker.clone(),
        workspace_manager.clone(),
        agent_runner,
    );

    let orch_tx = orchestrator.event_sender();

    // 7. Start workflow watcher.
    let watcher_tx = orch_tx.clone();
    let workflow_path = args.workflow_path.clone();
    tokio::spawn(async move {
        let (wtx, mut wrx) = tokio::sync::mpsc::channel(16);
        match symphony_workflow::watcher::WorkflowWatcher::new(
            workflow_path.clone(),
            wtx,
        ) {
            Ok(mut watcher) => {
                if let Err(e) = watcher.start().await {
                    tracing::error!(error = %e, "failed to start workflow watcher");
                    return;
                }
                while let Some(event) = wrx.recv().await {
                    match event {
                        symphony_workflow::watcher::WorkflowChangeEvent::Reloaded {
                            config,
                            prompt_template,
                        } => {
                            let _ = watcher_tx
                                .send(
                                    symphony_orchestrator::events::OrchestratorEvent::WorkflowReloaded {
                                        config,
                                        prompt_template,
                                    },
                                )
                                .await;
                        }
                        symphony_workflow::watcher::WorkflowChangeEvent::Error(msg) => {
                            tracing::error!(error = %msg, "workflow reload error");
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to create workflow watcher, live reload disabled"
                );
            }
        }
    });

    // 8. Optionally start HTTP server.
    let server_port = args.port.or(config.server_port);
    if let Some(port) = server_port {
        let snapshot_fn: Arc<dyn Fn() -> serde_json::Value + Send + Sync> =
            Arc::new(move || {
                serde_json::json!({
                    "generated_at": chrono::Utc::now().to_rfc3339(),
                    "counts": { "running": 0, "retrying": 0 },
                    "running": [],
                    "retrying": [],
                    "codex_totals": {
                        "input_tokens": 0,
                        "output_tokens": 0,
                        "total_tokens": 0,
                        "seconds_running": 0.0
                    },
                    "rate_limits": null
                })
            });

        let app_state = Arc::new(symphony_server::router::AppState {
            orchestrator_tx: orch_tx.clone(),
            snapshot_fn,
        });

        let router = symphony_server::router::create_router(app_state);
        tokio::spawn(async move {
            if let Err(e) = symphony_server::router::start_server(router, port).await {
                tracing::error!(error = %e, "HTTP server error");
            }
        });

        tracing::info!(port, "HTTP server started");
    }

    // 9. Run orchestrator (blocks until shutdown).
    tracing::info!("Symphony started, entering main loop");
    orchestrator.run().await;

    tracing::info!("Symphony shutdown complete");
    Ok(())
}

/// Parse an `"owner/repo"` project slug into its two components.
///
/// # Errors
///
/// Returns an error if the slug is not in `"owner/repo"` format.
fn parse_project_slug(slug: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = slug.split('/').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        anyhow::bail!(
            "Invalid project_slug format. Expected 'owner/repo', got '{}'",
            slug
        );
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_project_slug_valid() {
        let (owner, repo) = parse_project_slug("octocat/hello-world").unwrap();
        assert_eq!(owner, "octocat");
        assert_eq!(repo, "hello-world");
    }

    #[test]
    fn parse_project_slug_invalid_no_slash() {
        assert!(parse_project_slug("no-slash").is_err());
    }

    #[test]
    fn parse_project_slug_empty_parts() {
        assert!(parse_project_slug("/repo").is_err());
        assert!(parse_project_slug("owner/").is_err());
    }

    #[test]
    fn parse_project_slug_too_many_parts() {
        // "a/b/c" splits into 3 parts.
        assert!(parse_project_slug("a/b/c").is_err());
    }
}
