//! Symphony Workflow crate.
//!
//! Handles WORKFLOW.md parsing, configuration extraction, template rendering,
//! and file watching for dynamic reload.

pub mod config;
pub mod loader;
pub mod template;
pub mod validation;
pub mod watcher;
