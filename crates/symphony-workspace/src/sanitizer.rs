use regex::Regex;

/// Replace any character not in `[A-Za-z0-9._-]` with `_`.
///
/// This produces a filesystem-safe workspace key suitable for use as a
/// directory name under the workspace root.
pub fn sanitize_workspace_key(identifier: &str) -> String {
    let re = Regex::new(r"[^A-Za-z0-9._\-]").expect("invalid regex");
    re.replace_all(identifier, "_").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_prefix_replaced() {
        assert_eq!(sanitize_workspace_key("#123"), "_123");
    }

    #[test]
    fn dashes_preserved() {
        assert_eq!(sanitize_workspace_key("ABC-123"), "ABC-123");
    }

    #[test]
    fn slashes_and_spaces_replaced() {
        assert_eq!(sanitize_workspace_key("foo/bar baz"), "foo_bar_baz");
    }

    #[test]
    fn dots_and_underscores_preserved() {
        assert_eq!(sanitize_workspace_key("a.b_c"), "a.b_c");
    }

    #[test]
    fn empty_string_unchanged() {
        assert_eq!(sanitize_workspace_key(""), "");
    }

    #[test]
    fn all_special_characters_replaced() {
        assert_eq!(sanitize_workspace_key("!@#$%^&*()"), "__________");
    }

    #[test]
    fn purely_alphanumeric_unchanged() {
        assert_eq!(sanitize_workspace_key("abc123XYZ"), "abc123XYZ");
    }
}
