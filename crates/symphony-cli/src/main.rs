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

    // Determine auth: GitHub App or PAT.
    let app_token_provider: Option<Arc<symphony_tracker::github::app_token::GitHubAppTokenProvider>> =
        if let (Some(app_id), Some(installation_id), Some(key_path)) = (
            config.github_app_id,
            config.github_app_installation_id,
            config.github_app_private_key_path.as_deref(),
        ) {
            let private_key = std::fs::read_to_string(key_path).map_err(|e| {
                anyhow::anyhow!("failed to read GitHub App private key from {key_path}: {e}")
            })?;
            let app_config = symphony_tracker::github::app_token::GitHubAppConfig {
                app_id,
                installation_id,
                private_key_pem: private_key,
                api_endpoint: if config.tracker_endpoint.is_empty() {
                    "https://api.github.com".to_string()
                } else {
                    config.tracker_endpoint.clone()
                },
            };
            let provider =
                symphony_tracker::github::app_token::GitHubAppTokenProvider::new(app_config)
                    .map_err(|e| anyhow::anyhow!("failed to create GitHub App token provider: {e}"))?;
            tracing::info!("using GitHub App authentication (app_id={app_id})");
            Some(Arc::new(provider))
        } else {
            None
        };

    // Create the GitHub client with the appropriate auth method.
    let github_client = if let Some(ref provider) = app_token_provider {
        // GitHub App: pass the token provider so the client gets fresh tokens.
        symphony_tracker::github::client::GitHubClient::new_with_app(
            Arc::clone(provider),
            &config.tracker_project_slug,
            config.tracker_active_states.clone(),
            config.tracker_terminal_states.clone(),
            endpoint,
        )
    } else {
        if config.tracker_api_key.is_empty() {
            return Err(anyhow::anyhow!(
                "either tracker.api_key or GitHub App config (app_id + installation_id + private_key_path) is required"
            ));
        }
        // PAT: static token baked into the client.
        symphony_tracker::github::client::GitHubClient::new(
            &config.tracker_api_key,
            &config.tracker_project_slug,
            config.tracker_active_states.clone(),
            config.tracker_terminal_states.clone(),
            endpoint,
        )
    }
    .map_err(|e| {
        tracing::error!(error = %e, "failed to create GitHub client");
        anyhow::anyhow!("failed to create GitHub client: {e}")
    })?;

    let tracker: Arc<dyn symphony_tracker::traits::IssueTracker> =
        Arc::new(github_client);

    // If using GitHub App, set GH_TOKEN and GITHUB_TOKEN env vars so the
    // agent process and hooks can use gh CLI and git push.
    if let Some(ref provider) = app_token_provider {
        let token = provider.get_token().await.map_err(|e| {
            anyhow::anyhow!("failed to get GitHub App token for agent: {e}")
        })?;
        // Set env vars for child processes (hooks and codex agent).
        // SAFETY: We are single-threaded at this point during startup.
        unsafe {
            std::env::set_var("GH_TOKEN", &token);
            std::env::set_var("GITHUB_TOKEN", &token);
        }
        tracing::info!("set GH_TOKEN and GITHUB_TOKEN for agent subprocess");

        // Spawn a background task to refresh the token every 30 minutes.
        let refresh_provider = Arc::clone(provider);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(1800));
            loop {
                interval.tick().await;
                match refresh_provider.get_token().await {
                    Ok(new_token) => {
                        // SAFETY: env var mutation is the only way to pass
                        // refreshed tokens to child processes.
                        unsafe {
                            std::env::set_var("GH_TOKEN", &new_token);
                            std::env::set_var("GITHUB_TOKEN", &new_token);
                        }
                        tracing::info!("refreshed GitHub App token");
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "failed to refresh GitHub App token");
                    }
                }
            }
        });
    }

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
    let shared_snapshot = orchestrator.shared_snapshot();
    let server_port = args.port.or(config.server_port);
    if let Some(port) = server_port {
        let snapshot_handle = shared_snapshot.clone();
        let snapshot_fn: Arc<dyn Fn() -> serde_json::Value + Send + Sync> =
            Arc::new(move || {
                snapshot_handle
                    .read()
                    .map(|guard| guard.clone())
                    .unwrap_or_else(|_| serde_json::json!({"error": "snapshot lock poisoned"}))
            });

        let app_state = Arc::new(symphony_server::router::AppState {
            orchestrator_tx: orch_tx.clone(),
            snapshot_fn,
            approval_queue: orchestrator.approval_queue(),
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

