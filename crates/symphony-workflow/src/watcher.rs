//! File watching for dynamic WORKFLOW.md reload per Section 6.2.
//!
//! Watches a WORKFLOW.md file for changes, re-parses it on modification,
//! and sends events through a channel for the orchestrator to consume.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use tokio::sync::mpsc;
use tracing::{error, info, warn};

use symphony_core::domain::ServiceConfig;

use crate::config::build_config;
use crate::loader::load_workflow;

/// Event emitted when the watched WORKFLOW.md file changes.
#[derive(Debug, Clone)]
pub enum WorkflowChangeEvent {
    /// The workflow file was successfully re-parsed.
    Reloaded {
        /// Updated service configuration.
        config: ServiceConfig,
        /// Updated prompt template body.
        prompt_template: String,
    },
    /// The workflow file could not be re-parsed after a change.
    Error(String),
}

/// Watches a WORKFLOW.md file for modifications and emits reload events.
///
/// Uses the `notify` crate with a 100ms debounce window to avoid
/// duplicate events from editors that write files in multiple steps.
pub struct WorkflowWatcher {
    path: PathBuf,
    tx: mpsc::Sender<WorkflowChangeEvent>,
    running: Arc<AtomicBool>,
}

impl WorkflowWatcher {
    /// Create a new watcher for the given WORKFLOW.md path.
    ///
    /// Events are sent through `tx`. The watcher does not start
    /// observing until [`start`](Self::start) is called.
    pub fn new(
        path: PathBuf,
        tx: mpsc::Sender<WorkflowChangeEvent>,
    ) -> Result<Self, anyhow::Error> {
        if !path.exists() {
            anyhow::bail!("workflow file does not exist: {}", path.display());
        }

        Ok(Self {
            path,
            tx,
            running: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Start watching the file for changes.
    ///
    /// This method spawns a blocking background task that receives file
    /// system events and re-parses the workflow on each change. It returns
    /// immediately after starting the watcher.
    ///
    /// The background task runs until [`stop`](Self::stop) is called or the
    /// sender channel is closed. File change events are debounced by 100ms.
    pub async fn start(&mut self) -> Result<(), anyhow::Error> {
        self.running.store(true, Ordering::SeqCst);

        let (notify_tx, notify_rx) = std::sync::mpsc::channel();

        let debounce_duration = std::time::Duration::from_millis(100);
        let mut debouncer = new_debouncer(debounce_duration, notify_tx)?;

        let watch_path = self
            .path
            .parent()
            .unwrap_or_else(|| self.path.as_ref());

        debouncer.watcher().watch(
            watch_path,
            notify::RecursiveMode::NonRecursive,
        )?;

        info!(path = %self.path.display(), "started watching workflow file");

        let running = self.running.clone();
        let path = self.path.clone();
        let tx = self.tx.clone();
        let file_name = self
            .path
            .file_name()
            .map(|f| f.to_os_string())
            .unwrap_or_default();

        // Spawn a blocking task to receive from the sync channel.
        tokio::task::spawn_blocking(move || {
            while running.load(Ordering::SeqCst) {
                match notify_rx.recv_timeout(std::time::Duration::from_millis(250)) {
                    Ok(Ok(events)) => {
                        let dominated = events.iter().any(|e| {
                            e.kind == DebouncedEventKind::Any
                                && e.path.file_name() == Some(&file_name)
                        });

                        if !dominated {
                            continue;
                        }

                        info!(path = %path.display(), "workflow file changed, reloading");

                        let event = match reload_workflow(&path) {
                            Ok((config, prompt_template)) => {
                                info!("workflow reloaded successfully");
                                WorkflowChangeEvent::Reloaded {
                                    config,
                                    prompt_template,
                                }
                            }
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    "failed to reload workflow, keeping last known good config"
                                );
                                WorkflowChangeEvent::Error(e.to_string())
                            }
                        };

                        if tx.blocking_send(event).is_err() {
                            info!("channel closed, stopping watcher");
                            break;
                        }
                    }
                    Ok(Err(errs)) => {
                        error!(errors = ?errs, "file watcher error");
                        let msg = format!("file watcher error: {errs:?}");
                        let _ = tx.blocking_send(WorkflowChangeEvent::Error(msg));
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        continue;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        info!("file watcher channel disconnected, stopping");
                        break;
                    }
                }
            }

            info!(path = %path.display(), "workflow watcher stopped");
            drop(debouncer);
        });

        Ok(())
    }

    /// Signal the watcher to stop.
    ///
    /// The background task will exit on its next poll cycle (within ~250ms).
    pub fn stop(&mut self) {
        info!("stopping workflow watcher");
        self.running.store(false, Ordering::SeqCst);
    }
}

/// Re-read and re-parse the workflow file, returning the config and prompt.
fn reload_workflow(path: &PathBuf) -> Result<(ServiceConfig, String), anyhow::Error> {
    let workflow = load_workflow(path)?;
    let config = build_config(&workflow)?;
    Ok((config, workflow.prompt_template))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn new_fails_on_missing_file() {
        let (tx, _rx) = mpsc::channel(1);
        let result = WorkflowWatcher::new(PathBuf::from("/nonexistent/WORKFLOW.md"), tx);
        assert!(result.is_err());
    }

    #[test]
    fn new_succeeds_on_existing_file() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "hello").unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let result = WorkflowWatcher::new(tmp.path().to_path_buf(), tx);
        assert!(result.is_ok());
    }

    #[test]
    fn stop_sets_running_flag() {
        let mut tmp = NamedTempFile::new().unwrap();
        writeln!(tmp, "hello").unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let mut watcher = WorkflowWatcher::new(tmp.path().to_path_buf(), tx).unwrap();
        watcher.running.store(true, Ordering::SeqCst);
        watcher.stop();
        assert!(!watcher.running.load(Ordering::SeqCst));
    }

    #[test]
    fn reload_workflow_valid_file() {
        unsafe { std::env::set_var("SYMPHONY_WATCHER_TEST_KEY", "tok") };

        let mut tmp = NamedTempFile::new().unwrap();
        write!(
            tmp,
            "---\ntracker:\n  kind: github\n  api_key: $SYMPHONY_WATCHER_TEST_KEY\n  project_slug: o/r\n---\nHello prompt\n"
        )
        .unwrap();
        tmp.flush().unwrap();

        let (config, prompt) = reload_workflow(&tmp.path().to_path_buf()).unwrap();
        assert_eq!(config.tracker_kind, "github");
        assert_eq!(prompt, "Hello prompt");

        unsafe { std::env::remove_var("SYMPHONY_WATCHER_TEST_KEY") };
    }

    #[test]
    fn reload_workflow_invalid_yaml() {
        let mut tmp = NamedTempFile::new().unwrap();
        write!(tmp, "---\n[bad yaml:\n---\nbody\n").unwrap();
        tmp.flush().unwrap();

        let result = reload_workflow(&tmp.path().to_path_buf());
        assert!(result.is_err());
    }
}
