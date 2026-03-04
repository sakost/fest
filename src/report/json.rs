//! JSON report formatter.
//!
//! Serializes a [`MutationReport`] to a JSON string using `serde_json`.

use super::types::MutationReport;

/// Serialize a [`MutationReport`] to a pretty-printed JSON string.
///
/// # Errors
///
/// Returns [`crate::Error::Report`] if JSON serialization fails.
#[inline]
pub fn format_json(report: &MutationReport) -> Result<String, crate::Error> {
    serde_json::to_string_pretty(report)
        .map_err(|err| crate::Error::Report(format!("failed to serialize report to JSON: {err}")))
}
