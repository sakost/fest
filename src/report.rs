//! Report generation -- formatting and emitting mutation-testing results.
//!
//! Supports multiple output formats (text summary, JSON, etc.) and
//! returns formatted strings that the caller can write wherever needed.

/// JSON report formatter.
pub mod json;
/// Plain-text report formatter.
pub mod text;
/// Core types for mutation-testing reports.
pub mod types;

pub use types::MutationReport;

use crate::{Error, config::OutputFormat};

/// Format a mutation report in the given output format.
///
/// Returns the formatted report as a [`String`]. The caller decides where
/// to write the output (stdout, file, etc.).
///
/// # Errors
///
/// Returns [`Error::Report`] if formatting or serialization fails.
/// Returns [`Error::Report`] if the requested format is not yet implemented.
#[inline]
pub fn format_report(report: &MutationReport, format: &OutputFormat) -> Result<String, Error> {
    match *format {
        OutputFormat::Text => text::format_text(report),
        OutputFormat::Json => json::format_json(report),
        OutputFormat::Html => Err(Error::Report(
            "HTML report format is not yet implemented".to_owned(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use core::time::Duration;
    use std::path::PathBuf;

    use super::*;
    use crate::mutation::{Mutant, MutantResult, MutantStatus};

    /// Helper to create a test mutant with the given parameters.
    fn make_mutant(file: &str, line: u32, mutator: &str, original: &str, mutated: &str) -> Mutant {
        Mutant {
            file_path: PathBuf::from(file),
            line,
            column: 1_u32,
            byte_offset: 0_usize,
            byte_length: original.len(),
            original_text: original.to_owned(),
            mutated_text: mutated.to_owned(),
            mutator_name: mutator.to_owned(),
        }
    }

    /// Helper to create a `MutantResult` with the given status.
    fn make_result(mutant: Mutant, status: MutantStatus) -> MutantResult {
        MutantResult {
            mutant,
            status,
            tests_run: vec!["test_example".to_owned()],
            duration: Duration::from_millis(50_u64),
        }
    }

    // -- MutationReport::mutation_score tests --------------------------------

    /// Score is 0.0 when no mutants were tested.
    #[test]
    fn score_zero_when_none_tested() {
        let report =
            MutationReport::from_results(Vec::new(), 5_usize, 0_usize, Duration::from_secs(1_u64));
        assert!((report.mutation_score() - 0.0).abs() < f64::EPSILON);
    }

    /// Score is 100.0 when all tested mutants are killed.
    #[test]
    fn score_all_killed() {
        let results = vec![
            make_result(
                make_mutant("a.py", 1_u32, "arithmetic_op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 2_u32, "arithmetic_op", "-", "+"),
                MutantStatus::Killed,
            ),
        ];
        let report =
            MutationReport::from_results(results, 1_usize, 2_usize, Duration::from_secs(1_u64));
        assert!((report.mutation_score() - 100.0).abs() < f64::EPSILON);
    }

    /// Score is 0.0 when no mutants are killed.
    #[test]
    fn score_none_killed() {
        let results = vec![
            make_result(
                make_mutant("a.py", 1_u32, "arithmetic_op", "+", "-"),
                MutantStatus::Survived,
            ),
            make_result(
                make_mutant("a.py", 2_u32, "arithmetic_op", "-", "+"),
                MutantStatus::Survived,
            ),
        ];
        let report =
            MutationReport::from_results(results, 1_usize, 2_usize, Duration::from_secs(1_u64));
        assert!((report.mutation_score() - 0.0).abs() < f64::EPSILON);
    }

    /// Score is 50.0 when half the mutants are killed.
    #[test]
    fn score_half_killed() {
        let results = vec![
            make_result(
                make_mutant("a.py", 1_u32, "arithmetic_op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 2_u32, "arithmetic_op", "-", "+"),
                MutantStatus::Survived,
            ),
        ];
        let report =
            MutationReport::from_results(results, 1_usize, 2_usize, Duration::from_secs(1_u64));
        assert!((report.mutation_score() - 50.0).abs() < f64::EPSILON);
    }

    /// `NoCoverage` results are excluded from `mutants_tested`.
    #[test]
    fn no_coverage_excluded_from_tested_count() {
        let results = vec![
            make_result(
                make_mutant("a.py", 1_u32, "arithmetic_op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 2_u32, "arithmetic_op", "-", "+"),
                MutantStatus::NoCoverage,
            ),
        ];
        let report =
            MutationReport::from_results(results, 1_usize, 2_usize, Duration::from_secs(1_u64));
        assert_eq!(report.mutants_tested, 1_usize);
        assert_eq!(report.no_coverage, 1_usize);
        assert!((report.mutation_score() - 100.0).abs() < f64::EPSILON);
    }

    // -- MutationReport::from_results tests ----------------------------------

    /// `from_results` correctly counts all status categories.
    #[test]
    fn from_results_counts_all_statuses() {
        let results = vec![
            make_result(
                make_mutant("a.py", 1_u32, "op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 2_u32, "op", "-", "+"),
                MutantStatus::Survived,
            ),
            make_result(
                make_mutant("a.py", 3_u32, "op", "*", "/"),
                MutantStatus::Timeout,
            ),
            make_result(
                make_mutant("a.py", 4_u32, "op", "/", "*"),
                MutantStatus::NoCoverage,
            ),
            make_result(
                make_mutant("a.py", 5_u32, "op", "==", "!="),
                MutantStatus::Error("oops".to_owned()),
            ),
        ];

        let report =
            MutationReport::from_results(results, 3_usize, 10_usize, Duration::from_secs(5_u64));

        assert_eq!(report.files_scanned, 3_usize);
        assert_eq!(report.mutants_generated, 10_usize);
        assert_eq!(report.killed, 1_usize);
        assert_eq!(report.survived, 1_usize);
        assert_eq!(report.timeouts, 1_usize);
        assert_eq!(report.no_coverage, 1_usize);
        assert_eq!(report.errors, 1_usize);
        assert_eq!(report.mutants_tested, 4_usize);
        assert_eq!(report.results.len(), 5_usize);
        assert_eq!(report.duration, Duration::from_secs(5_u64));
    }

    /// `from_results` with an empty vector produces zero counts.
    #[test]
    fn from_results_empty() {
        let report =
            MutationReport::from_results(Vec::new(), 0_usize, 0_usize, Duration::from_secs(0_u64));
        assert_eq!(report.killed, 0_usize);
        assert_eq!(report.survived, 0_usize);
        assert_eq!(report.timeouts, 0_usize);
        assert_eq!(report.no_coverage, 0_usize);
        assert_eq!(report.errors, 0_usize);
        assert_eq!(report.mutants_tested, 0_usize);
    }

    // -- Threshold tests -----------------------------------------------------

    /// A threshold of 0.0 always passes.
    #[test]
    fn threshold_zero_always_passes() {
        let report =
            MutationReport::from_results(Vec::new(), 0_usize, 0_usize, Duration::from_secs(0_u64));
        assert!(report.passes_threshold(0.0));
    }

    /// Threshold passes when score equals the threshold.
    #[test]
    fn threshold_passes_when_equal() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "+", "-"),
            MutantStatus::Killed,
        )];
        let report =
            MutationReport::from_results(results, 1_usize, 1_usize, Duration::from_secs(1_u64));
        // score is 100.0
        assert!(report.passes_threshold(100.0));
    }

    /// Threshold fails when score is below the threshold.
    #[test]
    fn threshold_fails_when_below() {
        let results = vec![
            make_result(
                make_mutant("a.py", 1_u32, "op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 2_u32, "op", "-", "+"),
                MutantStatus::Survived,
            ),
        ];
        let report =
            MutationReport::from_results(results, 1_usize, 2_usize, Duration::from_secs(1_u64));
        // score is 50.0
        assert!(!report.passes_threshold(80.0));
    }

    // -- Text reporter tests -------------------------------------------------

    /// Text report contains the header.
    #[test]
    fn text_report_contains_header() {
        let report =
            MutationReport::from_results(Vec::new(), 0_usize, 0_usize, Duration::from_secs(0_u64));
        let output = text::format_text(&report).expect("should format text");
        assert!(output.contains("fest mutation testing report"));
        assert!(output.contains("----------------------------"));
    }

    /// Text report contains statistics.
    #[test]
    fn text_report_contains_statistics() {
        let results = vec![
            make_result(
                make_mutant("src/parser.py", 42_u32, "ArithmeticOp", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant(
                    "src/parser.py",
                    87_u32,
                    "NegateCondition",
                    "if valid:",
                    "if not valid:",
                ),
                MutantStatus::Survived,
            ),
        ];
        let report =
            MutationReport::from_results(results, 12_usize, 347_usize, Duration::from_secs(30_u64));
        let output = text::format_text(&report).expect("should format text");

        assert!(output.contains("Files scanned:"));
        assert!(output.contains("12"));
        assert!(output.contains("Mutants generated:"));
        assert!(output.contains("347"));
        assert!(output.contains("Killed:"));
        assert!(output.contains("Survived:"));
    }

    /// Text report lists survived mutants with file, line, mutator, and text.
    #[test]
    fn text_report_lists_survived() {
        let results = vec![make_result(
            make_mutant("src/parser.py", 42_u32, "ArithmeticOp", "x + 1", "x - 1"),
            MutantStatus::Survived,
        )];
        let report =
            MutationReport::from_results(results, 1_usize, 1_usize, Duration::from_secs(1_u64));
        let output = text::format_text(&report).expect("should format text");

        assert!(output.contains("Survived mutants:"));
        assert!(output.contains("src/parser.py:42"));
        assert!(output.contains("ArithmeticOp"));
        assert!(output.contains("`x + 1` -> `x - 1`"));
    }

    /// Text report does not list survived section when there are none.
    #[test]
    fn text_report_no_survived_section_when_all_killed() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "+", "-"),
            MutantStatus::Killed,
        )];
        let report =
            MutationReport::from_results(results, 1_usize, 1_usize, Duration::from_secs(1_u64));
        let output = text::format_text(&report).expect("should format text");

        assert!(!output.contains("Survived mutants:"));
    }

    /// Text report shows no-coverage count when present.
    #[test]
    fn text_report_shows_no_coverage() {
        let results = vec![
            make_result(
                make_mutant("a.py", 1_u32, "op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 2_u32, "op", "-", "+"),
                MutantStatus::NoCoverage,
            ),
        ];
        let report =
            MutationReport::from_results(results, 1_usize, 2_usize, Duration::from_secs(1_u64));
        let output = text::format_text(&report).expect("should format text");

        assert!(output.contains("no coverage"));
    }

    // -- JSON reporter tests -------------------------------------------------

    /// JSON report is valid JSON and contains expected fields.
    #[test]
    fn json_report_contains_fields() {
        let results = vec![
            make_result(
                make_mutant("a.py", 1_u32, "arithmetic_op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 2_u32, "comparison_op", "==", "!="),
                MutantStatus::Survived,
            ),
        ];
        let report =
            MutationReport::from_results(results, 3_usize, 10_usize, Duration::from_secs(5_u64));
        let json_str = json::format_json(&report).expect("should format JSON");

        // Verify it is valid JSON by parsing it
        let value: serde_json::Value =
            serde_json::from_str(&json_str).expect("should be valid JSON");

        assert_eq!(value["files_scanned"], 3_i64);
        assert_eq!(value["mutants_generated"], 10_i64);
        assert_eq!(value["killed"], 1_i64);
        assert_eq!(value["survived"], 1_i64);
        assert_eq!(value["mutants_tested"], 2_i64);
    }

    /// JSON report contains individual results with mutant details.
    #[test]
    fn json_report_contains_mutant_details() {
        let results = vec![make_result(
            make_mutant("module.py", 10_u32, "boolean_op", "and", "or"),
            MutantStatus::Killed,
        )];
        let report =
            MutationReport::from_results(results, 1_usize, 1_usize, Duration::from_secs(1_u64));
        let json_str = json::format_json(&report).expect("should format JSON");
        let value: serde_json::Value =
            serde_json::from_str(&json_str).expect("should be valid JSON");

        let results_array = value["results"]
            .as_array()
            .expect("results should be array");
        assert_eq!(results_array.len(), 1_usize);

        let first = &results_array[0_usize];
        assert_eq!(first["mutant"]["original_text"], "and");
        assert_eq!(first["mutant"]["mutated_text"], "or");
        assert_eq!(first["mutant"]["mutator_name"], "boolean_op");
        assert_eq!(first["mutant"]["line"], 10_i64);
        assert_eq!(first["status"], "Killed");
    }

    /// JSON report for empty results.
    #[test]
    fn json_report_empty_results() {
        let report =
            MutationReport::from_results(Vec::new(), 0_usize, 0_usize, Duration::from_secs(0_u64));
        let json_str = json::format_json(&report).expect("should format JSON");
        let value: serde_json::Value =
            serde_json::from_str(&json_str).expect("should be valid JSON");

        assert_eq!(value["killed"], 0_i64);
        assert_eq!(value["survived"], 0_i64);
        let results_array = value["results"]
            .as_array()
            .expect("results should be array");
        assert!(results_array.is_empty());
    }

    // -- format_report dispatch tests ----------------------------------------

    /// `format_report` dispatches to text formatter.
    #[test]
    fn format_report_text() {
        let report =
            MutationReport::from_results(Vec::new(), 0_usize, 0_usize, Duration::from_secs(0_u64));
        let output = format_report(&report, &OutputFormat::Text).expect("should format text");
        assert!(output.contains("fest mutation testing report"));
    }

    /// `format_report` dispatches to JSON formatter.
    #[test]
    fn format_report_json() {
        let report =
            MutationReport::from_results(Vec::new(), 0_usize, 0_usize, Duration::from_secs(0_u64));
        let output = format_report(&report, &OutputFormat::Json).expect("should format JSON");
        let _value: serde_json::Value =
            serde_json::from_str(&output).expect("should be valid JSON");
    }

    /// `format_report` returns an error for HTML (not yet implemented).
    #[test]
    fn format_report_html_not_implemented() {
        let report =
            MutationReport::from_results(Vec::new(), 0_usize, 0_usize, Duration::from_secs(0_u64));
        let result = format_report(&report, &OutputFormat::Html);
        assert!(result.is_err());
    }
}
