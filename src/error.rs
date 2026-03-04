//! Top-level error types for fest.

/// All error variants that fest can produce.
#[derive(Debug, thiserror::Error)]
#[allow(
    clippy::error_impl_error,
    reason = "this is the canonical crate-level error type"
)]
pub enum Error {
    /// An error originating from configuration parsing or validation.
    #[error("config error: {0}")]
    Config(String),

    /// An error originating from the mutation engine.
    #[error("mutation error: {0}")]
    Mutation(String),

    /// An error originating from coverage analysis.
    #[error("coverage error: {0}")]
    Coverage(String),

    /// An error originating from the test runner.
    #[error("runner error: {0}")]
    Runner(String),

    /// An error originating from report generation.
    #[error("report error: {0}")]
    Report(String),

    /// An I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}
