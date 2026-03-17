//! Per-issue activity log for the orchestrator.
//!
//! Stores a bounded, per-issue ring buffer of activity entries that are
//! included in the orchestrator snapshot and surfaced through the HTTP API.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// A single activity entry for an issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub event_type: String,
    pub message: String,
    pub timestamp: String,
}

/// Thread-safe, bounded activity log keyed by issue ID.
///
/// Each issue has its own ring buffer of entries. When the buffer exceeds
/// `max_entries`, the oldest entry is evicted.
pub struct ActivityLog {
    entries: Mutex<HashMap<String, VecDeque<ActivityEntry>>>,
    max_entries: usize,
}

impl ActivityLog {
    /// Create a new activity log with the given maximum entries per issue.
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            max_entries,
        }
    }

    /// Push a new entry for the given issue, evicting the oldest if full.
    pub fn push(&self, issue_id: &str, entry: ActivityEntry) {
        let mut guard = self.entries.lock().expect("activity log lock poisoned");
        let deque = guard
            .entry(issue_id.to_string())
            .or_insert_with(VecDeque::new);
        if deque.len() >= self.max_entries {
            deque.pop_front();
        }
        deque.push_back(entry);
    }

    /// Get all entries for the given issue, oldest first.
    pub fn get_entries(&self, issue_id: &str) -> Vec<ActivityEntry> {
        let guard = self.entries.lock().expect("activity log lock poisoned");
        guard
            .get(issue_id)
            .map(|d| d.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Remove all entries for the given issue.
    pub fn remove_issue(&self, issue_id: &str) {
        let mut guard = self.entries.lock().expect("activity log lock poisoned");
        guard.remove(issue_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(event_type: &str, message: &str) -> ActivityEntry {
        ActivityEntry {
            event_type: event_type.to_string(),
            message: message.to_string(),
            timestamp: "2026-03-17T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn push_and_get_entries() {
        let log = ActivityLog::new(100);
        log.push("issue-1", make_entry("session_started", "Session began"));
        log.push("issue-1", make_entry("turn_completed", "Turn 1 done"));

        let entries = log.get_entries("issue-1");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].event_type, "session_started");
        assert_eq!(entries[1].event_type, "turn_completed");
    }

    #[test]
    fn get_entries_returns_empty_for_unknown_issue() {
        let log = ActivityLog::new(100);
        let entries = log.get_entries("nonexistent");
        assert!(entries.is_empty());
    }

    #[test]
    fn eviction_when_full() {
        let log = ActivityLog::new(3);
        log.push("issue-1", make_entry("e1", "first"));
        log.push("issue-1", make_entry("e2", "second"));
        log.push("issue-1", make_entry("e3", "third"));
        log.push("issue-1", make_entry("e4", "fourth"));

        let entries = log.get_entries("issue-1");
        assert_eq!(entries.len(), 3);
        // The first entry should have been evicted.
        assert_eq!(entries[0].event_type, "e2");
        assert_eq!(entries[1].event_type, "e3");
        assert_eq!(entries[2].event_type, "e4");
    }

    #[test]
    fn remove_issue_clears_entries() {
        let log = ActivityLog::new(100);
        log.push("issue-1", make_entry("e1", "msg"));
        log.push("issue-2", make_entry("e2", "msg"));

        log.remove_issue("issue-1");

        assert!(log.get_entries("issue-1").is_empty());
        assert_eq!(log.get_entries("issue-2").len(), 1);
    }

    #[test]
    fn multiple_issues_are_independent() {
        let log = ActivityLog::new(100);
        log.push("issue-1", make_entry("e1", "msg1"));
        log.push("issue-2", make_entry("e2", "msg2"));
        log.push("issue-1", make_entry("e3", "msg3"));

        assert_eq!(log.get_entries("issue-1").len(), 2);
        assert_eq!(log.get_entries("issue-2").len(), 1);
    }

    #[test]
    fn max_entries_of_one() {
        let log = ActivityLog::new(1);
        log.push("issue-1", make_entry("e1", "first"));
        log.push("issue-1", make_entry("e2", "second"));

        let entries = log.get_entries("issue-1");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].event_type, "e2");
    }
}
