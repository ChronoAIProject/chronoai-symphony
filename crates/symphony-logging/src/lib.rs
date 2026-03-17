//! Symphony Logging crate.
//!
//! Provides opinionated tracing initialization for the Symphony orchestrator.
//! Two modes are available:
//!
//! - **JSON mode** (`init_logging`): structured JSON output suitable for
//!   production log aggregation pipelines (ELK, Datadog, etc.).
//! - **Pretty mode** (`init_logging_pretty`): human-readable colored output
//!   for local development.

pub mod setup;
