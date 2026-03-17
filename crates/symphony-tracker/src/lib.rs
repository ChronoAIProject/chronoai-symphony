//! Symphony Tracker crate.
//!
//! Provides the `IssueTracker` trait and concrete implementations for
//! fetching and managing issues from external trackers.

pub mod github;
pub mod traits;

pub use traits::IssueTracker;
