use regex::Regex;

/// Replace any character not in `[A-Za-z0-9._-]` with `_`.
pub fn sanitize_workspace_key(identifier: &str) -> String {
    let re = Regex::new(r"[^A-Za-z0-9._\-]").expect("invalid regex");
    re.replace_all(identifier, "_").into_owned()
}

/// Compose a session ID from a thread ID and turn ID.
pub fn compose_session_id(thread_id: &str, turn_id: &str) -> String {
    format!("{thread_id}-{turn_id}")
}

/// Normalize a state string to lowercase.
pub fn normalize_state(state: &str) -> String {
    state.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_workspace_key_preserves_valid_chars() {
        assert_eq!(sanitize_workspace_key("abc-123.def_ghi"), "abc-123.def_ghi");
    }

    #[test]
    fn sanitize_workspace_key_replaces_hash() {
        assert_eq!(sanitize_workspace_key("#123"), "_123");
    }

    #[test]
    fn sanitize_workspace_key_replaces_spaces_and_special() {
        assert_eq!(sanitize_workspace_key("my issue / test"), "my_issue___test");
    }

    #[test]
    fn sanitize_workspace_key_handles_empty_string() {
        assert_eq!(sanitize_workspace_key(""), "");
    }

    #[test]
    fn compose_session_id_basic() {
        assert_eq!(compose_session_id("thread-1", "turn-2"), "thread-1-turn-2");
    }

    #[test]
    fn compose_session_id_empty_parts() {
        assert_eq!(compose_session_id("", ""), "-");
    }

    #[test]
    fn normalize_state_lowercase() {
        assert_eq!(normalize_state("In Progress"), "in progress");
    }

    #[test]
    fn normalize_state_already_lowercase() {
        assert_eq!(normalize_state("todo"), "todo");
    }

    #[test]
    fn normalize_state_mixed_case() {
        assert_eq!(normalize_state("IN_PROGRESS"), "in_progress");
    }
}
