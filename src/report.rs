//! Report generation -- formatting and emitting mutation-testing results.
//!
//! Supports multiple output formats (text summary, JSON, etc.) and
//! returns formatted strings that the caller can write wherever needed.

/// HTML report formatter.
pub mod html;
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
/// When `colored` is true **and** the format supports it (currently only
/// [`OutputFormat::Text`]), ANSI colors are used to highlight the output.
///
/// Returns the formatted report as a [`String`]. The caller decides where
/// to write the output (stdout, file, etc.).
///
/// # Errors
///
/// Returns [`Error::Report`] if formatting or serialization fails.
/// Returns [`Error::Report`] if the requested format is not yet implemented.
#[inline]
pub fn format_report(
    report: &MutationReport,
    format: &OutputFormat,
    colored: bool,
) -> Result<String, Error> {
    match *format {
        OutputFormat::Text => text::format_text(report, colored),
        OutputFormat::Json => json::format_json(report),
        OutputFormat::Html => html::format_html(report),
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
        let report = MutationReport::from_results(
            Vec::new(),
            5_usize,
            0_usize,
            Duration::from_secs(1_u64),
            None,
        );
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
        let report = MutationReport::from_results(
            results,
            1_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
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
        let report = MutationReport::from_results(
            results,
            1_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
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
        let report = MutationReport::from_results(
            results,
            1_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
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
        let report = MutationReport::from_results(
            results,
            1_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
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

        let report = MutationReport::from_results(
            results,
            3_usize,
            10_usize,
            Duration::from_secs(5_u64),
            None,
        );

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
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
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
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        assert!(report.passes_threshold(0.0));
    }

    /// Threshold passes when score equals the threshold.
    #[test]
    fn threshold_passes_when_equal() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "+", "-"),
            MutantStatus::Killed,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
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
        let report = MutationReport::from_results(
            results,
            1_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
        // score is 50.0
        assert!(!report.passes_threshold(80.0));
    }

    // -- Text reporter tests -------------------------------------------------

    /// Text report contains the header.
    #[test]
    fn text_report_contains_header() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output = text::format_text(&report, false).expect("should format text");
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
        let report = MutationReport::from_results(
            results,
            12_usize,
            347_usize,
            Duration::from_secs(30_u64),
            None,
        );
        let output = text::format_text(&report, false).expect("should format text");

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
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = text::format_text(&report, false).expect("should format text");

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
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = text::format_text(&report, false).expect("should format text");

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
        let report = MutationReport::from_results(
            results,
            1_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = text::format_text(&report, false).expect("should format text");

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
        let report = MutationReport::from_results(
            results,
            3_usize,
            10_usize,
            Duration::from_secs(5_u64),
            None,
        );
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
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
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
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
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
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output =
            format_report(&report, &OutputFormat::Text, false).expect("should format text");
        assert!(output.contains("fest mutation testing report"));
    }

    /// `format_report` dispatches to JSON formatter.
    #[test]
    fn format_report_json() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output =
            format_report(&report, &OutputFormat::Json, false).expect("should format JSON");
        let _value: serde_json::Value =
            serde_json::from_str(&output).expect("should be valid JSON");
    }

    /// `format_report` dispatches to HTML formatter.
    #[test]
    fn format_report_html() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output =
            format_report(&report, &OutputFormat::Html, false).expect("should format HTML");
        assert!(output.contains("<!DOCTYPE html>"));
        assert!(output.contains("fest mutation testing report"));
    }

    // -- HTML reporter tests -------------------------------------------------

    /// HTML report contains the DOCTYPE declaration.
    #[test]
    fn html_report_contains_doctype() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("<!DOCTYPE html>"));
    }

    /// HTML report contains the page title.
    #[test]
    fn html_report_contains_title() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("<title>fest mutation testing report</title>"));
    }

    /// HTML report contains inline CSS styles.
    #[test]
    fn html_report_contains_inline_css() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("<style>"));
        assert!(output.contains(".killed"));
        assert!(output.contains(".survived"));
        assert!(output.contains(".no-coverage"));
        assert!(output.contains("</style>"));
    }

    /// HTML report contains summary statistics.
    #[test]
    fn html_report_contains_summary() {
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
        let report = MutationReport::from_results(
            results,
            3_usize,
            10_usize,
            Duration::from_secs(5_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");

        assert!(output.contains("Files scanned"));
        assert!(output.contains("Mutants generated"));
        assert!(output.contains("Killed"));
        assert!(output.contains("Survived"));
        assert!(output.contains("Mutation score"));
    }

    /// HTML report contains the mutation score as a percentage.
    #[test]
    fn html_report_contains_score_percentage() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "+", "-"),
            MutantStatus::Killed,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("100.0%"));
    }

    /// HTML report shows file path in file section.
    #[test]
    fn html_report_shows_file_path() {
        let results = vec![make_result(
            make_mutant("src/parser.py", 10_u32, "ArithmeticOp", "+", "-"),
            MutantStatus::Killed,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("src/parser.py"));
    }

    /// HTML report uses green class for killed mutants.
    #[test]
    fn html_report_killed_line_green() {
        let results = vec![make_result(
            make_mutant("a.py", 5_u32, "op", "+", "-"),
            MutantStatus::Killed,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("class=\"line killed\""));
    }

    /// HTML report uses red class for survived mutants.
    #[test]
    fn html_report_survived_line_red() {
        let results = vec![make_result(
            make_mutant("a.py", 5_u32, "op", "+", "-"),
            MutantStatus::Survived,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("class=\"line survived\""));
    }

    /// HTML report uses grey class for no-coverage mutants.
    #[test]
    fn html_report_no_coverage_line_grey() {
        let results = vec![make_result(
            make_mutant("a.py", 5_u32, "op", "+", "-"),
            MutantStatus::NoCoverage,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("class=\"line no-coverage\""));
    }

    /// A line with mixed killed and survived uses survived (red) class.
    #[test]
    fn html_report_mixed_survived_takes_priority() {
        let results = vec![
            make_result(
                make_mutant("a.py", 5_u32, "op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 5_u32, "op", "-", "+"),
                MutantStatus::Survived,
            ),
        ];
        let report = MutationReport::from_results(
            results,
            1_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("class=\"line survived\""));
    }

    /// A line with killed and no-coverage uses no-coverage (grey) class.
    #[test]
    fn html_report_mixed_killed_and_no_coverage_uses_grey() {
        let results = vec![
            make_result(
                make_mutant("a.py", 5_u32, "op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 5_u32, "op", "-", "+"),
                MutantStatus::NoCoverage,
            ),
        ];
        let report = MutationReport::from_results(
            results,
            1_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        // Not all killed, so should be grey (no-coverage class).
        assert!(output.contains("class=\"line no-coverage\""));
    }

    /// HTML report shows mutation detail with mutator name and texts.
    #[test]
    fn html_report_shows_mutation_detail() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "ArithmeticOp", "+", "-"),
            MutantStatus::Killed,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("ArithmeticOp"));
        assert!(output.contains("[KILLED]"));
        assert!(output.contains("<code>+</code>"));
        assert!(output.contains("<code>-</code>"));
    }

    /// HTML report shows survived mutation detail label.
    #[test]
    fn html_report_shows_survived_label() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "+", "-"),
            MutantStatus::Survived,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("[SURVIVED]"));
    }

    /// HTML report shows timeout mutation detail label.
    #[test]
    fn html_report_shows_timeout_label() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "+", "-"),
            MutantStatus::Timeout,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("[TIMEOUT]"));
    }

    /// HTML report shows error mutation detail label.
    #[test]
    fn html_report_shows_error_label() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "+", "-"),
            MutantStatus::Error("segfault".to_owned()),
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("[ERROR]"));
    }

    /// HTML report escapes HTML special characters in source text.
    #[test]
    fn html_report_escapes_html_chars() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "a < b", "a > b"),
            MutantStatus::Killed,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("a &lt; b"));
        assert!(output.contains("a &gt; b"));
    }

    /// HTML report escapes ampersands in source text.
    #[test]
    fn html_report_escapes_ampersand() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "a & b", "a | b"),
            MutantStatus::Killed,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("a &amp; b"));
    }

    /// HTML report escapes double quotes in source text.
    #[test]
    fn html_report_escapes_double_quotes() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "x == \"hello\"", "x != \"hello\""),
            MutantStatus::Killed,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("&quot;hello&quot;"));
    }

    /// HTML report escapes single quotes in source text.
    #[test]
    fn html_report_escapes_single_quotes() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "x == 'world'", "x != 'world'"),
            MutantStatus::Killed,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("&#39;world&#39;"));
    }

    /// HTML report groups mutations by file.
    #[test]
    fn html_report_groups_by_file() {
        let results = vec![
            make_result(
                make_mutant("alpha.py", 1_u32, "op", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("beta.py", 1_u32, "op", "+", "-"),
                MutantStatus::Survived,
            ),
        ];
        let report = MutationReport::from_results(
            results,
            2_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("alpha.py"));
        assert!(output.contains("beta.py"));
    }

    /// HTML report for empty results produces valid HTML structure.
    #[test]
    fn html_report_empty_results() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("<!DOCTYPE html>"));
        assert!(output.contains("</html>"));
        assert!(output.contains("0.0%"));
    }

    /// HTML report is self-contained (no external stylesheets).
    #[test]
    fn html_report_self_contained() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        // Should not contain external stylesheet links.
        assert!(!output.contains("<link rel=\"stylesheet\""));
        // Should contain inline styles.
        assert!(output.contains("<style>"));
    }

    /// HTML report contains closing tags in the right order.
    #[test]
    fn html_report_closing_tags() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("</body>"));
        assert!(output.contains("</html>"));
        // Body should close before html.
        let body_close = output.find("</body>");
        let html_close = output.find("</html>");
        assert!(body_close < html_close);
    }

    /// HTML report shows multiple mutations on the same line.
    #[test]
    fn html_report_multiple_mutations_same_line() {
        let results = vec![
            make_result(
                make_mutant("a.py", 5_u32, "ArithmeticOp", "+", "-"),
                MutantStatus::Killed,
            ),
            make_result(
                make_mutant("a.py", 5_u32, "ArithmeticOp", "+", "*"),
                MutantStatus::Killed,
            ),
        ];
        let report = MutationReport::from_results(
            results,
            1_usize,
            2_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        // Should show both mutation details.
        assert!(output.contains("<code>-</code>"));
        assert!(output.contains("<code>*</code>"));
        // But only one line header for line 5 (showing the original source text).
        let line5_count = output.matches("<span class=\"line-num\">5</span>").count();
        assert_eq!(line5_count, 1_usize);
    }

    /// HTML report handles all status types in summary correctly.
    #[test]
    fn html_report_all_statuses_in_summary() {
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
        let report = MutationReport::from_results(
            results,
            1_usize,
            5_usize,
            Duration::from_secs(2_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("Timeout"));
        assert!(output.contains("Errors"));
        assert!(output.contains("No coverage"));
    }

    /// HTML report shows `[NO COVERAGE]` label in mutation detail.
    #[test]
    fn html_report_shows_no_coverage_label() {
        let results = vec![make_result(
            make_mutant("a.py", 1_u32, "op", "+", "-"),
            MutantStatus::NoCoverage,
        )];
        let report = MutationReport::from_results(
            results,
            1_usize,
            1_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("[NO COVERAGE]"));
    }

    /// Text report shows seed in header and statistics when set.
    #[test]
    fn text_report_shows_seed() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            Some(42_u64),
        );
        let output = text::format_text(&report, false).expect("should format text");
        assert!(output.contains("(seed: 42)"));
        assert!(output.contains("Seed:               42"));
    }

    /// Text report omits seed line when seed is None.
    #[test]
    fn text_report_no_seed_line_when_none() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output = text::format_text(&report, false).expect("should format text");
        assert!(!output.contains("Seed:"));
        assert!(!output.contains("seed:"));
    }

    /// HTML report shows seed in header and summary when set.
    #[test]
    fn html_report_shows_seed() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            Some(123_u64),
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(output.contains("(seed: 123)"));
        assert!(output.contains("<tr><td>Seed</td><td>123</td></tr>"));
    }

    /// HTML report omits seed row when seed is None.
    #[test]
    fn html_report_no_seed_when_none() {
        let report = MutationReport::from_results(
            Vec::new(),
            0_usize,
            0_usize,
            Duration::from_secs(0_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        assert!(!output.contains("Seed"));
    }

    /// HTML report has balanced `<div>` and `</div>` tags.
    #[test]
    fn html_report_div_tags_balanced() {
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
                make_mutant("b.py", 3_u32, "op", "*", "/"),
                MutantStatus::NoCoverage,
            ),
        ];
        let report = MutationReport::from_results(
            results,
            2_usize,
            3_usize,
            Duration::from_secs(1_u64),
            None,
        );
        let output = html::format_html(&report).expect("should format HTML");
        let open_divs = output.matches("<div").count();
        let close_divs = output.matches("</div>").count();
        assert_eq!(open_divs, close_divs);
    }
}
