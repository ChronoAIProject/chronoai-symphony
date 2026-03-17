//! Tracing subscriber initialization.
//!
//! Call one of the two public functions once at program startup to configure
//! the global tracing subscriber. Calling either function more than once will
//! panic because `tracing` only allows a single global default.

use tracing_subscriber::EnvFilter;

/// Initialize the global tracing subscriber with JSON-formatted output.
///
/// The log level is controlled by the `RUST_LOG` environment variable.
/// If `RUST_LOG` is not set, defaults to `info`.
///
/// Output includes:
/// - Target module path
/// - Thread IDs
/// - Source file and line number
///
/// # Panics
///
/// Panics if a global subscriber has already been set.
pub fn init_logging() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .json()
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to set tracing subscriber");
}

/// Initialize the global tracing subscriber with human-readable output.
///
/// Behaves identically to [`init_logging`] but uses a pretty-printed format
/// with ANSI colors, which is easier to read during local development.
///
/// # Panics
///
/// Panics if a global subscriber has already been set.
pub fn init_logging_pretty() {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .pretty()
        .with_target(true)
        .with_thread_ids(true)
        .with_file(true)
        .with_line_number(true)
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("failed to set tracing subscriber");
}

#[cfg(test)]
mod tests {
    // Note: We cannot test init_logging / init_logging_pretty in unit tests
    // because setting the global subscriber is a one-time operation per process.
    // Integration tests or manual verification are used instead.

    use tracing_subscriber::EnvFilter;

    #[test]
    fn default_env_filter_is_info() {
        // Verify that the fallback filter parses correctly.
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info"));
        let formatted = format!("{filter}");
        assert!(
            formatted.contains("info"),
            "expected filter to contain 'info', got: {formatted}"
        );
    }
}
