//! Symphony Server crate.
//!
//! Provides an HTTP server with a REST API and a live HTML dashboard for
//! monitoring the orchestrator. Built on Axum.
//!
//! # Endpoints
//!
//! | Method | Path                         | Description                 |
//! |--------|------------------------------|-----------------------------|
//! | GET    | `/`                          | HTML dashboard              |
//! | GET    | `/api/v1/state`              | Full orchestrator snapshot   |
//! | GET    | `/api/v1/{issue_identifier}` | Single issue details        |
//! | POST   | `/api/v1/refresh`            | Trigger a poll+reconcile    |

pub mod api;
pub mod dashboard;
pub mod router;
